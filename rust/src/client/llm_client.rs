//! One entry point for every model call. Picks candidates through the router, talks to the right wire protocol,
//! retries transient failures with jittered backoff, and falls back to the next candidate when a model is unusable.
//! Fallbacks are announced as Notice events, never silent.
use super::adapters::*;
use super::errors::{classify, ProviderError};
use super::providers::{split, Provider, PRESET_NAMES};
use super::response::{StreamEvent, TokenUsage, ToolCall};
use super::routing::{ModelRegistry, Router};
use crate::config::{Config, ConfigHandle};
use crate::util::trace::Tracer;
use futures::stream::BoxStream;
use futures::StreamExt;
use rand::RngExt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Sampling each open-model family was tuned for (first match on the model id wins). Qwen drifts and repeats itself at
/// 1.0; Kimi, MiniMax and Gemini want top_p 0.95 with it.
const SAMPLING: &[(&str, f64, Option<f64>)] =
    &[("qwen", 0.55, Some(1.0)), ("kimi", 1.0, Some(0.95)), ("minimax", 1.0, Some(0.95)), ("gemini", 1.0, Some(0.95)), ("glm", 1.0, None)];

/// (temperature, top_p) for one call: a configured temperature wins, then the family table, then the server default.
pub fn sampling(model: &str, configured: Option<f64>) -> (Option<f64>, Option<f64>) {
    if let Some(t) = configured {
        return (Some(t), None);
    }
    let low = model.to_lowercase();
    SAMPLING.iter().find(|(k, _, _)| low.contains(k)).map_or((None, None), |(_, t, p)| (Some(*t), *p))
}

/// The switch that turns a model's thinking down for an easy request and up for a hard one, in the wording of the
/// server it goes to. `effort` is "low" (easy), "high" (hard) or None (leave the server's default alone).
/// The Command Code gateway ignores all of these (probed 2026-10-22), so nothing is sent there.
pub fn thinking_body(provider: &str, model: &str, effort: Option<&str>) -> Value {
    let Some(effort) = effort.filter(|e| !e.is_empty()) else { return json!({}) };
    if provider == "gateway" || provider == "codex" {
        return json!({});
    }
    let on = effort != "low";
    let low = model.to_lowercase();
    if low.contains("gpt-oss") {
        return json!({"reasoning_effort": effort});
    }
    match provider {
        "deepseek" => return json!({"thinking": {"type": if on { "enabled" } else { "disabled" }}}),
        "zai" => return json!({"thinking": if on { json!({"type": "enabled", "clear_thinking": false}) } else { json!({"type": "disabled"}) }}),
        "openrouter" => return json!({"reasoning": {"effort": effort}}),
        "together" => return json!({"reasoning": {"enabled": on}}),
        _ => {}
    }
    if !PRESET_NAMES.contains(&provider) || provider == "lmstudio" {
        // your own vLLM / SGLang / LM Studio server: the switch lives in the model's chat template
        if low.contains("deepseek") {
            return json!({"chat_template_kwargs": {"thinking": on}});
        }
        if low.contains("qwen") || low.contains("glm") {
            return json!({"chat_template_kwargs": {"enable_thinking": on}});
        }
    }
    json!({})
}

/// Size of the request by part (characters, so an estimate): shows what the prompt is made of without storing any of it.
pub fn request_chars(messages: &[Value]) -> Value {
    let (mut system, mut user, mut assistant, mut tool_calls, mut tool_results) = (0, 0, 0, 0, 0);
    for m in messages {
        let text = m["content"].as_str().unwrap_or("").chars().count();
        match m["role"].as_str().unwrap_or("") {
            "tool" => tool_results += text,
            "system" => system += text,
            "user" => user += text,
            _ => {
                assistant += text;
                tool_calls += m["tool_calls"]
                    .as_array()
                    .map(|a| a.iter().map(|tc| tc["function"]["arguments"].as_str().unwrap_or("").len() + tc["function"]["name"].as_str().unwrap_or("").len()).sum::<usize>())
                    .unwrap_or(0);
            }
        }
    }
    json!({"system": system, "user": user, "assistant": assistant, "tool_calls": tool_calls, "tool_results": tool_results})
}

/// Seconds to wait before retrying. A busy upstream (429, 503) needs longer than a dropped connection: the free MiniMax
/// tier answered 429 three times in four seconds, then the harness fell back to another model and lost the prompt cache.
pub fn backoff(attempt: usize, status: Option<u16>) -> f64 {
    let mut rng = rand::rng();
    if matches!(status, Some(429) | Some(503)) {
        return [3.0, 8.0, 20.0][attempt.min(2)] * rng.random_range(0.8..1.2);
    }
    (2f64.powi(attempt as i32)).min(30.0) * rng.random_range(0.5..1.0)
}

#[derive(Default)]
struct State {
    no_temperature: HashSet<String>,
    max_out: HashMap<String, usize>, // model -> output cap its server accepts (0 = send none)
    no_thinking: HashSet<String>,    // models whose server refused the thinking switch
    quota_blocked: HashSet<String>,  // providers out of quota this session
    last_model: Option<String>,
    auto_effort: Option<String>,
}

pub struct LLMClient {
    pub config: ConfigHandle,
    pub tracer: Arc<Tracer>,
    pub registry: Arc<ModelRegistry>,
    router: Router,
    pub(crate) adapters: Mutex<HashMap<String, Arc<dyn Adapter>>>,
    state: Mutex<State>,
    max_retries: usize,
    /// Tests only: replies played back instead of calling a model, and every request seen.
    #[cfg(test)]
    pub script: Mutex<Option<std::collections::VecDeque<Vec<StreamEvent>>>>,
    #[cfg(test)]
    pub requests: Mutex<Vec<Vec<Value>>>,
}

impl LLMClient {
    pub fn new(config: ConfigHandle, tracer: Arc<Tracer>) -> LLMClient {
        let registry = Arc::new(ModelRegistry::new(config.clone(), crate::config::loader::get_data_dir()));
        LLMClient { router: Router::new(config.clone()), config, tracer, registry, adapters: Mutex::new(HashMap::new()), state: Mutex::new(State::default()), max_retries: 3,
            #[cfg(test)] script: Mutex::new(None), #[cfg(test)] requests: Mutex::new(Vec::new()) }
    }

    pub async fn initialize(&self) {
        self.registry.load().await;
    }

    pub fn last_model(&self) -> Option<String> {
        self.state.lock().unwrap().last_model.clone()
    }

    pub fn set_auto_effort(&self, e: Option<&str>) {
        self.state.lock().unwrap().auto_effort = e.map(String::from);
    }

    /// Which account a model is billed to: models sharing one also share its quota.
    pub(crate) fn provider_of(&self, model: &str) -> String {
        let cfg = self.config.get();
        match split(&cfg, model) {
            Some((p, _)) => p.name,
            None if model.starts_with("codex/") => "codex".into(),
            None => "gateway".into(),
        }
    }

    /// Straight to a provider (`groq:...`), with its own URL and key.
    fn direct_adapter(&self, p: &Provider) -> Arc<dyn Adapter> {
        let mut a = self.adapters.lock().unwrap();
        a.entry(p.name.clone())
            .or_insert_with(|| {
                let key = p.api_key().unwrap_or_default();
                if p.protocol == "anthropic" {
                    Arc::new(AnthropicAdapter::new(&key, &p.base_url, p.headers.clone()))
                } else {
                    Arc::new(OpenAIChatAdapter::new(&key, &p.base_url, p.headers.clone()))
                }
            })
            .clone()
    }

    fn gateway_adapter(&self, endpoint: &str) -> Arc<dyn Adapter> {
        let cfg = self.config.get();
        let mut a = self.adapters.lock().unwrap();
        a.entry(endpoint.to_string())
            .or_insert_with(|| match endpoint {
                "/responses" => Arc::new(CodexResponsesAdapter::new(cfg.reasoning_effort.clone())),
                e => {
                    let headers = if cfg.zdr { [("x-cmd-zdr".to_string(), "1".to_string())].into() } else { Default::default() };
                    let key = cfg.api_key().unwrap_or_default();
                    if e == "/messages" {
                        Arc::new(AnthropicAdapter::new(&key, &cfg.base_url(), headers))
                    } else {
                        Arc::new(OpenAIChatAdapter::new(&key, &cfg.base_url(), headers))
                    }
                }
            })
            .clone()
    }

    pub fn chat_completion(self: &Arc<Self>, messages: Vec<Value>, tools: Option<Vec<Value>>, stream: bool, role: &str) -> BoxStream<'static, StreamEvent> {
        let this = self.clone();
        let role = role.to_string();
        Box::pin(async_stream::stream! {
            #[cfg(test)]
            {
                this.requests.lock().unwrap().push(messages.clone());
                let next = this.script.lock().unwrap().as_mut().map(|q| q.pop_front());
                if let Some(turn) = next {
                    for ev in turn.unwrap_or_else(|| vec![StreamEvent::Error("script ran out".into())]) { yield ev; }
                    return;
                }
            }
            let est_tokens = serde_json::to_string(&messages).map(|s| s.len()).unwrap_or(0) / 3;
            let (models, skipped) = this.router.candidates(&this.registry, &role, tools.is_some(), est_tokens);
            let blocked = this.state.lock().unwrap().quota_blocked.clone();
            let models: Vec<String> = models.into_iter().filter(|m| !blocked.contains(&this.provider_of(m))).collect();
            for s in &skipped { this.tracer.emit("model_skipped", json!({"role": role, "reason": s})); }
            if models.is_empty() {
                let why = if skipped.is_empty() { "no models configured".to_string() } else { skipped.join("; ") };
                yield StreamEvent::Error(format!("No usable model for role '{role}' ({why})"));
                return;
            }
            let mut last_error = "unknown error".to_string();
            let mut queue: std::collections::VecDeque<String> = models.into();
            let mut prev: Option<String> = None;
            while let Some(model) = queue.pop_front() {
                if let Some(p) = &prev {
                    yield StreamEvent::Notice(format!("Falling back from {p} to {model} ({})", crate::util::text::head_chars(&last_error, 120)));
                }
                prev = Some(model.clone());
                let mut emitted = false;
                let mut outcome: Option<ProviderError> = None;
                let mut inner = this.call_model(model.clone(), role.clone(), messages.clone(), tools.clone(), stream, est_tokens);
                while let Some(item) = inner.next().await {
                    match item {
                        Ok(ev) => { if matches!(ev, StreamEvent::TextDelta(_) | StreamEvent::ToolCallStart{..} | StreamEvent::ToolCallDelta{..} | StreamEvent::ToolCallComplete(_)) && stream { emitted = true; } yield ev; }
                        Err((e, did_emit)) => { emitted |= did_emit; outcome = Some(e); }
                    }
                }
                let Some(outcome) = outcome else { return };
                last_error = format!("{model}: {outcome}");
                if outcome.quota {
                    // every model on this provider shares the quota: skip the rest of them
                    let p = this.provider_of(&model);
                    this.state.lock().unwrap().quota_blocked.insert(p.clone());
                    queue.retain(|m| this.provider_of(m) != p);
                }
                let om = outcome.to_string();
                if om.contains("unsupported_model") || om.contains("not supported on this endpoint") {
                    let (_, near) = this.registry.validate(&model);
                    last_error.push_str(". Not in the model catalog");
                    if near.is_empty() { last_error.push('.'); } else { last_error.push_str(&format!("; did you mean: {}?", near.join(", "))); }
                }
                if emitted || !outcome.fallback {
                    // output already reached the caller (a fallback would duplicate it), or the request itself is bad
                    yield StreamEvent::Error(last_error);
                    return;
                }
            }
            yield StreamEvent::Error(format!("All candidate models failed. Last: {last_error}"));
        })
    }

    /// max_tokens for this call: the configured cap, what the server said it accepts, and the room left in the context,
    /// whichever is smallest. e.g. a 128k window with 110k of prompt leaves 128k-110k-4k = 14k to write.
    pub(crate) fn output_cap(&self, model: &str, window: usize, est_tokens: usize) -> Option<usize> {
        let cfg = self.config.get();
        let st = self.state.lock().unwrap();
        let configured = st.max_out.get(model).copied().unwrap_or(cfg.model.max_output_tokens);
        let room = window.saturating_sub(est_tokens).saturating_sub(4096).max(1024);
        let cap = configured.min(room);
        (cap > 0).then_some(cap)
    }

    /// One model, with retries. Errors carry whether any output already reached the caller.
    fn call_model(self: &Arc<Self>, model: String, role: String, messages: Vec<Value>, tools: Option<Vec<Value>>, stream: bool, est_tokens: usize) -> BoxStream<'static, Result<StreamEvent, (ProviderError, bool)>> {
        let this = self.clone();
        Box::pin(async_stream::stream! {
            let cfg: Arc<Config> = this.config.get();
            let info = this.registry.get(&model);
            let direct = split(&cfg, &model);
            if let Some((p, _)) = &direct {
                if p.api_key().is_none() {
                    // say which env var is missing instead of a bare 401 from the provider
                    let mut e = ProviderError::new(cfg.missing_key(Some(&model)).unwrap_or_default(), Some(401));
                    e.fallback = true;
                    yield Err((e, false));
                    return;
                }
            }
            let adapter: Arc<dyn Adapter> = match &direct { Some((p, _)) => this.direct_adapter(p), None => this.gateway_adapter(info.endpoint()) };
            let wire_model = direct.as_ref().map(|d| d.1.clone()).unwrap_or_else(|| model.clone());
            let (mut temperature, mut top_p) = if this.state.lock().unwrap().no_temperature.contains(&model) { (None, None) } else { sampling(&model, cfg.model.temperature) };
            // summaries never need deep thinking; everything else follows the request (an explicit config value wins)
            let effort = if role == "summarize" { Some("low".to_string()) } else { cfg.reasoning_effort.clone().or_else(|| this.state.lock().unwrap().auto_effort.clone()) };
            let provider = this.provider_of(&model);
            let mut emitted = false;

            let mut attempt = 0;
            loop {
                let started = Instant::now();
                let mut first_token: Option<f64> = None;
                let (mut text, mut calls, mut usage, mut finish, mut reasoning): (String, Vec<ToolCall>, Option<TokenUsage>, Option<String>, Option<String>) = (String::new(), vec![], None, None, None);
                let is_chat = adapter.is_openai_chat();
                let cap = if is_chat { this.output_cap(&model, info.context_length, est_tokens) } else { None };
                let body = if this.state.lock().unwrap().no_thinking.contains(&model) { json!({}) } else { thinking_body(&provider, &wire_model, effort.as_deref()) };
                let req = StreamReq {
                    model: wire_model.clone(), messages: messages.clone(), tools: tools.clone(),
                    temperature, max_tokens: cap, top_p: if is_chat { top_p } else { None },
                    extra_body: if is_chat && body.as_object().map_or(false, |o| !o.is_empty()) { Some(body.clone()) } else { None },
                    effort: effort.clone(),
                };
                let had_body = req.extra_body.is_some();
                let mut failure: Option<ProviderError> = None;
                {
                    let mut s = adapter.stream(req);
                    while let Some(item) = s.next().await {
                        match item {
                            Err(e) => { failure = Some(e); break; }
                            Ok(ev) => {
                                if first_token.is_none() && matches!(ev, StreamEvent::TextDelta(_) | StreamEvent::ToolCallStart { .. }) {
                                    first_token = Some(started.elapsed().as_secs_f64());
                                }
                                match &ev {
                                    StreamEvent::TextDelta(t) => text.push_str(t),
                                    StreamEvent::ToolCallComplete(c) => calls.push(c.clone()),
                                    StreamEvent::MessageComplete { finish_reason, usage: u, reasoning: r, .. } => {
                                        usage = u.clone(); finish = finish_reason.clone(); reasoning = r.clone();
                                        continue; // re-emitted below, in the requested shape
                                    }
                                    _ => {}
                                }
                                if stream { emitted = true; yield Ok(ev); }
                            }
                        }
                    }
                }
                let Some(mut e) = failure else {
                    this.state.lock().unwrap().last_model = Some(model.clone());
                    this.tracer.emit("model_call", json!({
                        "model": model, "role": role, "attempt": attempt, "ok": true,
                        "sec": (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
                        "ttft": first_token.map(|t| (t * 1000.0).round() / 1000.0),
                        "tokens": usage.as_ref().map(|u| u.to_value()), "tool_calls": calls.len(),
                        "request_chars": request_chars(&messages),
                    }));
                    if stream {
                        yield Ok(StreamEvent::MessageComplete { finish_reason: finish, usage, reasoning, text: None });
                    } else {
                        for c in calls { yield Ok(StreamEvent::ToolCallComplete(c)); }
                        yield Ok(StreamEvent::MessageComplete { finish_reason: finish, usage, reasoning, text: (!text.is_empty()).then_some(text) });
                    }
                    return;
                };
                if e.message.is_empty() { e = classify(None, "empty error"); }
                this.tracer.emit("model_call", json!({
                    "model": model, "role": role, "attempt": attempt, "ok": false, "status": e.status,
                    "error": crate::util::text::head_chars(&e.message, 300), "sec": (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
                }));
                if e.drop_thinking && had_body && !emitted {
                    this.state.lock().unwrap().no_thinking.insert(model.clone()); // this server has no such switch
                    continue;
                }
                if e.drop_max_tokens && cap.is_some() && !emitted {
                    // "valid range of max_tokens is [1, 8192]" -> 8192; no usable number -> stop sending one
                    let c = cap.unwrap();
                    let n = crate::util::re::lit(r"\d+").clone();
                    let _ = n;
                    let nums: Vec<usize> = e.message.split(|ch: char| !ch.is_ascii_digit()).filter_map(|s| s.parse().ok()).filter(|n| *n >= 1024 && *n < c).collect();
                    this.state.lock().unwrap().max_out.insert(model.clone(), nums.into_iter().max().unwrap_or(0));
                    continue;
                }
                if e.drop_temperature && (temperature.is_some() || top_p.is_some()) && !emitted {
                    this.state.lock().unwrap().no_temperature.insert(model.clone());
                    temperature = None;
                    top_p = None;
                    continue;
                }
                if e.retry && attempt < this.max_retries && !emitted {
                    yield Ok(StreamEvent::Notice(format!("{model} didn't answer ({}), trying again ({}/{})", crate::util::text::head_chars(&e.message, 80), attempt + 1, this.max_retries)));
                    tokio::time::sleep(Duration::from_secs_f64(backoff(attempt, e.status))).await;
                    attempt += 1;
                    continue;
                }
                if e.fallback {
                    this.registry.mark_down(&model, if matches!(e.status, Some(403) | Some(404)) { 3600.0 } else { 60.0 });
                }
                yield Err((e, emitted));
                return;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn families() {
        assert_eq!(sampling("Qwen3-coder", None), (Some(0.55), Some(1.0)));
        assert_eq!(sampling("gpt-5", None), (None, None));
        assert_eq!(sampling("qwen", Some(0.2)), (Some(0.2), None));
        assert_eq!(thinking_body("gateway", "x", Some("high")), json!({}));
        assert_eq!(thinking_body("deepseek", "x", Some("low")), json!({"thinking": {"type": "disabled"}}));
    }
}
