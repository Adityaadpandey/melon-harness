//! Wire-format adapters. Each one turns our OpenAI-style message history into one provider protocol and streams back
//! normalized StreamEvents. Nothing in here knows about routing, retries or fallback; that lives in LLMClient.
use super::codex_auth::CodexAuth;
use super::errors::{classify, ProviderError};
use super::response::{parse_tool_call_arguments, StreamEvent, TokenUsage, ToolCall};
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

pub type EventStream<'a> = BoxStream<'a, Result<StreamEvent, ProviderError>>;

#[derive(Clone, Debug, Default)]
pub struct StreamReq {
    pub model: String,
    pub messages: Vec<Value>,
    pub tools: Option<Vec<Value>>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<usize>,
    pub top_p: Option<f64>,
    /// Provider-specific fields merged into the request body, e.g. the thinking switch.
    pub extra_body: Option<Value>,
    pub effort: Option<String>,
}

#[async_trait]
pub trait Adapter: Send + Sync {
    fn stream(&self, req: StreamReq) -> EventStream<'_>;
    fn is_openai_chat(&self) -> bool {
        false
    }
}

fn empty_schema() -> Value {
    json!({"type": "object", "properties": {}})
}

fn tool_defs(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({"type": "function", "function": {
                "name": t["name"], "description": t.get("description").cloned().unwrap_or(json!("")),
                "parameters": t.get("parameters").cloned().unwrap_or_else(empty_schema)}})
        })
        .collect()
}

const REASONING_FIELDS: [&str; 3] = ["reasoning_content", "reasoning", "reasoning_text"];

/// History as this server should see it. Thinking goes back to the model that wrote it, on every turn: DeepSeek, Kimi
/// and GLM lose the thread of a tool loop, or answer 400, without it. Another model can't read it, so a switch drops it.
pub fn wire_messages(messages: &[Value], model: &str) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            if let Some(o) = m.as_object_mut() {
                if let Some(by) = o.remove("reasoning_model") {
                    let by = by.as_str().unwrap_or("");
                    if !(by == model || by.ends_with(&format!(":{model}"))) {
                        o.remove("reasoning_content");
                    }
                }
            }
            m
        })
        .collect()
}

fn http_client(read_timeout: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(read_timeout))
        .build()
        .expect("http client")
}

fn conn_err(e: reqwest::Error) -> ProviderError {
    classify(None, &format!("Connection error: {e}"))
}

/// The `data:` payloads of a server-sent-events body, one string each.
fn sse_data(resp: reqwest::Response) -> BoxStream<'static, Result<String, ProviderError>> {
    let mut bytes = resp.bytes_stream();
    Box::pin(async_stream::try_stream! {
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(conn_err)?;
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let line = line.trim_end_matches(['\r', '\n']);
                if let Some(rest) = line.strip_prefix("data:") {
                    yield rest.trim().to_string();
                }
            }
        }
        if !buf.is_empty() {
            let line = String::from_utf8_lossy(&buf).to_string();
            if let Some(rest) = line.trim().strip_prefix("data:") {
                yield rest.trim().to_string();
            }
        }
    })
}

async fn fail(resp: reqwest::Response) -> ProviderError {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    // keep the server's own message (usually {"error": {"message": ...}}) readable
    let msg = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().or_else(|| v["message"].as_str()).map(String::from))
        .unwrap_or(body);
    classify(Some(status), &format!("Error code: {status} - {msg}"))
}

// ---------------------------------------------------------------- OpenAI chat

pub struct OpenAIChatAdapter {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    headers: BTreeMap<String, String>,
}

impl OpenAIChatAdapter {
    pub fn new(api_key: &str, base_url: &str, headers: BTreeMap<String, String>) -> OpenAIChatAdapter {
        OpenAIChatAdapter { client: http_client(120), api_key: api_key.into(), base_url: base_url.trim_end_matches('/').into(), headers }
    }
}

/// Which streamed tool call a delta belongs to. Most servers number them; some open-model servers send index=None, and
/// keying on that merged two parallel calls into one call with garbled arguments.
fn call_slot(calls: &BTreeMap<i64, (String, String, String)>, index: Option<i64>, id: &str) -> i64 {
    if let Some(i) = index {
        return i;
    }
    if !id.is_empty() {
        for (slot, c) in calls {
            if c.0 == id {
                return *slot;
            }
        }
    }
    if !id.is_empty() || calls.is_empty() {
        return calls.keys().max().map_or(0, |m| m + 1); // a new id starts a new call
    }
    *calls.keys().max().unwrap() // no id and no index: a continuation of the latest call
}

#[async_trait]
impl Adapter for OpenAIChatAdapter {
    fn is_openai_chat(&self) -> bool {
        true
    }

    fn stream(&self, req: StreamReq) -> EventStream<'_> {
        Box::pin(async_stream::try_stream! {
            let mut body = json!({
                "model": req.model, "messages": wire_messages(&req.messages, &req.model),
                "stream": true, "stream_options": {"include_usage": true},
            });
            let b = body.as_object_mut().unwrap();
            if let Some(t) = req.temperature { b.insert("temperature".into(), json!(t)); }
            if let Some(m) = req.max_tokens.filter(|m| *m > 0) { b.insert("max_tokens".into(), json!(m)); }
            if let Some(p) = req.top_p { b.insert("top_p".into(), json!(p)); }
            if let Some(Value::Object(extra)) = &req.extra_body { for (k, v) in extra { b.insert(k.clone(), v.clone()); } }
            if let Some(tools) = req.tools.as_ref().filter(|t| !t.is_empty()) {
                b.insert("tools".into(), json!(tool_defs(tools)));
                b.insert("tool_choice".into(), json!("auto"));
            }
            let mut rb = self.client.post(format!("{}/chat/completions", self.base_url)).bearer_auth(&self.api_key).json(&body);
            for (k, v) in &self.headers { rb = rb.header(k, v); }
            let resp = rb.send().await.map_err(conn_err)?;
            if !resp.status().is_success() { Err(fail(resp).await)?; unreachable!() }

            let mut finish: Option<String> = None;
            let mut usage: Option<TokenUsage> = None;
            let mut calls: BTreeMap<i64, (String, String, String)> = BTreeMap::new();
            let mut reasoning = String::new();
            let mut data = sse_data(resp);
            while let Some(payload) = data.next().await {
                let payload = payload?;
                if payload == "[DONE]" || payload.is_empty() { continue; }
                let Ok(chunk) = serde_json::from_str::<Value>(&payload) else { continue };
                if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
                    Err(classify(None, &format!("stream error: {}", err)))?;
                }
                if let Some(u) = chunk.get("usage").filter(|u| u.is_object()) {
                    let g = |k: &str| u[k].as_i64().unwrap_or(0);
                    // OpenAI nests it; DeepSeek says prompt_cache_hit_tokens; Kimi puts cached_tokens at the top level
                    let cached = [u["prompt_tokens_details"]["cached_tokens"].as_i64().unwrap_or(0), g("prompt_cache_hit_tokens"), g("cached_tokens")]
                        .into_iter().find(|n| *n > 0).unwrap_or(0);
                    usage = Some(TokenUsage { prompt_tokens: g("prompt_tokens"), completion_tokens: g("completion_tokens"), total_tokens: g("total_tokens"), cached_tokens: cached, cache_write_tokens: 0 });
                }
                let Some(choice) = chunk["choices"].get(0) else { continue };
                if let Some(f) = choice["finish_reason"].as_str() { finish = Some(f.to_string()); }
                let delta = &choice["delta"];
                // DeepSeek, Kimi, GLM and Qwen stream their thinking beside the answer, under one of these names
                if let Some(t) = REASONING_FIELDS.iter().find_map(|f| delta[*f].as_str().filter(|s| !s.is_empty())) { reasoning.push_str(t); }
                if let Some(c) = delta["content"].as_str().filter(|s| !s.is_empty()) {
                    yield StreamEvent::TextDelta(c.to_string());
                }
                for d in delta["tool_calls"].as_array().into_iter().flatten() {
                    let id = d["id"].as_str().unwrap_or("");
                    let slot = call_slot(&calls, d["index"].as_i64(), id);
                    let call = calls.entry(slot).or_insert_with(|| (String::new(), String::new(), String::new()));
                    if !id.is_empty() { call.0 = id.to_string(); }
                    let f = &d["function"];
                    if let Some(n) = f["name"].as_str().filter(|n| !n.is_empty()) {
                        if call.1.is_empty() {
                            call.1 = n.to_string();
                            yield StreamEvent::ToolCallStart { call_id: call.0.clone(), name: n.to_string() };
                        }
                    }
                    if let Some(a) = f["arguments"].as_str().filter(|a| !a.is_empty()) {
                        call.2.push_str(a);
                        yield StreamEvent::ToolCallDelta { call_id: call.0.clone(), arguments_delta: a.to_string() };
                    }
                }
            }
            for (idx, c) in calls {
                let id = if c.0.is_empty() { format!("call_{idx}") } else { c.0 };
                yield StreamEvent::ToolCallComplete(ToolCall { call_id: id, name: c.1, arguments: parse_tool_call_arguments(&c.2) });
            }
            yield StreamEvent::MessageComplete { finish_reason: finish, usage, reasoning: (!reasoning.is_empty()).then_some(reasoning), text: None };
        })
    }
}

// ------------------------------------------------------------------ Anthropic

/// OpenAI-style history -> (system blocks, anthropic messages). Anthropic wants tool results as blocks inside the *next
/// user* message, and rejects any tool_use that has no matching tool_result. Histories that were pruned, compacted or
/// resumed can contain such orphans, so we patch them with a placeholder rather than failing the whole request.
pub fn to_anthropic_messages(messages: &[Value]) -> (Option<Vec<Value>>, Vec<Value>) {
    let system: Vec<Value> = messages
        .iter()
        .filter(|m| m["role"] == "system" && m["content"].as_str().map_or(false, |s| !s.is_empty()))
        .map(|m| json!({"type": "text", "text": m["content"]}))
        .collect();
    let mut out: Vec<Value> = vec![];
    let mut pending: Vec<String> = vec![]; // tool_use ids still waiting for a result

    fn push_user_blocks(out: &mut Vec<Value>, blocks: Vec<Value>) {
        if let Some(last) = out.last_mut() {
            if last["role"] == "user" {
                last["content"].as_array_mut().unwrap().extend(blocks);
                return;
            }
        }
        out.push(json!({"role": "user", "content": blocks}));
    }
    fn settle(out: &mut Vec<Value>, pending: &mut Vec<String>) {
        if !pending.is_empty() {
            let blocks = pending.drain(..).map(|i| json!({"type": "tool_result", "tool_use_id": i, "content": "[no result recorded]", "is_error": true})).collect();
            push_user_blocks(out, blocks);
        }
    }

    for m in messages {
        match m["role"].as_str().unwrap_or("") {
            "system" => continue,
            "tool" => {
                let tid = m["tool_call_id"].as_str().unwrap_or("").to_string();
                pending.retain(|p| *p != tid);
                let content = m["content"].as_str().filter(|s| !s.is_empty()).unwrap_or("(no output)");
                push_user_blocks(&mut out, vec![json!({"type": "tool_result", "tool_use_id": tid, "content": content})]);
            }
            "assistant" => {
                settle(&mut out, &mut pending);
                let mut blocks = vec![];
                if let Some(t) = m["content"].as_str().filter(|s| !s.is_empty()) {
                    blocks.push(json!({"type": "text", "text": t}));
                }
                for tc in m["tool_calls"].as_array().into_iter().flatten() {
                    let args = &tc["function"]["arguments"];
                    let parsed = match args {
                        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(json!({})),
                        v => v.clone(),
                    };
                    let id = tc["id"].as_str().unwrap_or("").to_string();
                    blocks.push(json!({"type": "tool_use", "id": id, "name": tc["function"]["name"],
                                       "input": if parsed.is_object() { parsed } else { json!({}) }}));
                    pending.push(id);
                }
                if !blocks.is_empty() {
                    out.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            _ => {
                settle(&mut out, &mut pending);
                let t = m["content"].as_str().filter(|s| !s.is_empty()).unwrap_or("(empty)");
                push_user_blocks(&mut out, vec![json!({"type": "text", "text": t})]);
            }
        }
    }
    (if system.is_empty() { None } else { Some(system) }, out)
}

pub struct AnthropicAdapter {
    client: reqwest::Client,
    api_key: String,
    root: String,
    headers: BTreeMap<String, String>,
}

impl AnthropicAdapter {
    const MAX_TOKENS: usize = 16384;

    pub fn new(api_key: &str, base_url: &str, headers: BTreeMap<String, String>) -> AnthropicAdapter {
        // /v1/messages is appended below, so strip a trailing /v1
        let mut root = base_url.trim_end_matches('/').to_string();
        if let Some(r) = root.strip_suffix("/v1") {
            root = r.to_string();
        }
        AnthropicAdapter { client: http_client(120), api_key: api_key.into(), root, headers }
    }
}

#[async_trait]
impl Adapter for AnthropicAdapter {
    fn stream(&self, req: StreamReq) -> EventStream<'_> {
        Box::pin(async_stream::try_stream! {
            let (system, mut msgs) = to_anthropic_messages(&req.messages);
            let mut body = json!({"model": req.model, "max_tokens": Self::MAX_TOKENS, "stream": true});
            // second breakpoint: everything up to the newest message is a stable prefix on the next turn
            if let Some(last) = msgs.last_mut().and_then(|m| m["content"].as_array_mut()).and_then(|c| c.last_mut()) {
                last["cache_control"] = json!({"type": "ephemeral"});
            }
            if let Some(mut system) = system {
                // cache the stable prefix; agent loops resend it every turn
                if let Some(l) = system.last_mut() { l["cache_control"] = json!({"type": "ephemeral"}); }
                body["system"] = json!(system);
            }
            body["messages"] = json!(msgs);
            if let Some(tools) = req.tools.as_ref().filter(|t| !t.is_empty()) {
                body["tools"] = json!(tools.iter().map(|t| json!({
                    "name": t["name"], "description": t.get("description").cloned().unwrap_or(json!("")),
                    "input_schema": t.get("parameters").cloned().unwrap_or_else(empty_schema)})).collect::<Vec<_>>());
            }
            // no temperature: the current Claude models reject sampling params
            let mut rb = self.client.post(format!("{}/v1/messages", self.root)).header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01").json(&body);
            for (k, v) in &self.headers { rb = rb.header(k, v); }
            let resp = rb.send().await.map_err(conn_err)?;
            if !resp.status().is_success() { Err(fail(resp).await)?; unreachable!() }

            // block index -> (id, name, partial json)
            let mut tools: BTreeMap<i64, (String, String, String)> = BTreeMap::new();
            let (mut input, mut output, mut cached, mut created) = (0i64, 0i64, 0i64, 0i64);
            let mut stop: Option<String> = None;
            let mut data = sse_data(resp);
            while let Some(payload) = data.next().await {
                let payload = payload?;
                let Ok(ev) = serde_json::from_str::<Value>(&payload) else { continue };
                match ev["type"].as_str().unwrap_or("") {
                    "message_start" => {
                        let u = &ev["message"]["usage"];
                        input = u["input_tokens"].as_i64().unwrap_or(0);
                        cached = u["cache_read_input_tokens"].as_i64().unwrap_or(0);
                        created = u["cache_creation_input_tokens"].as_i64().unwrap_or(0);
                        output = u["output_tokens"].as_i64().unwrap_or(0);
                    }
                    "content_block_start" if ev["content_block"]["type"] == "tool_use" => {
                        let (id, name) = (ev["content_block"]["id"].as_str().unwrap_or("").to_string(), ev["content_block"]["name"].as_str().unwrap_or("").to_string());
                        tools.insert(ev["index"].as_i64().unwrap_or(0), (id.clone(), name.clone(), String::new()));
                        yield StreamEvent::ToolCallStart { call_id: id, name };
                    }
                    "content_block_delta" => {
                        let d = &ev["delta"];
                        match d["type"].as_str().unwrap_or("") {
                            "text_delta" => { if let Some(t) = d["text"].as_str().filter(|t| !t.is_empty()) { yield StreamEvent::TextDelta(t.to_string()); } }
                            "input_json_delta" => {
                                if let Some(t) = tools.get_mut(&ev["index"].as_i64().unwrap_or(0)) {
                                    let part = d["partial_json"].as_str().unwrap_or("");
                                    t.2.push_str(part);
                                    yield StreamEvent::ToolCallDelta { call_id: t.0.clone(), arguments_delta: part.to_string() };
                                }
                            }
                            _ => {}
                        }
                    }
                    "message_delta" => {
                        if let Some(s) = ev["delta"]["stop_reason"].as_str() { stop = Some(s.to_string()); }
                        if let Some(o) = ev["usage"]["output_tokens"].as_i64() { output = o; }
                    }
                    "error" => { Err(classify(None, &format!("stream error: {}", ev["error"])))?; }
                    _ => {}
                }
            }
            for (_, (id, name, args)) in tools {
                let arguments = if args.trim().is_empty() { Map::new() } else { parse_tool_call_arguments(&args) };
                yield StreamEvent::ToolCallComplete(ToolCall { call_id: id, name, arguments });
            }
            let prompt = input + cached + created;
            let finish = match stop.as_deref() { Some("tool_use") => Some("tool_calls".to_string()), Some("end_turn") => Some("stop".to_string()), o => o.map(String::from) };
            yield StreamEvent::MessageComplete {
                finish_reason: finish,
                usage: Some(TokenUsage { prompt_tokens: prompt, completion_tokens: output, total_tokens: prompt + output, cached_tokens: cached, cache_write_tokens: created }),
                reasoning: None, text: None,
            };
        })
    }
}

// ---------------------------------------------------------------- Codex (Responses API)

/// OpenAI chat history -> (instructions, Responses API input items). System messages become `instructions`; tool calls
/// and their results become function_call / function_call_output items linked by call_id.
pub fn to_responses_input(messages: &[Value]) -> (String, Vec<Value>) {
    let instructions: Vec<&str> = messages.iter().filter(|m| m["role"] == "system").filter_map(|m| m["content"].as_str()).filter(|s| !s.is_empty()).collect();
    let mut items = vec![];
    for m in messages {
        match m["role"].as_str().unwrap_or("") {
            "system" => continue,
            "tool" => items.push(json!({"type": "function_call_output", "call_id": m["tool_call_id"],
                                        "output": m["content"].as_str().filter(|s| !s.is_empty()).unwrap_or("(no output)")})),
            "assistant" => {
                if let Some(t) = m["content"].as_str().filter(|s| !s.is_empty()) {
                    items.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": t}]}));
                }
                for tc in m["tool_calls"].as_array().into_iter().flatten() {
                    let args = match &tc["function"]["arguments"] {
                        Value::String(s) => s.clone(),
                        v => v.to_string(),
                    };
                    items.push(json!({"type": "function_call", "call_id": tc["id"], "name": tc["function"]["name"], "arguments": args}));
                }
            }
            _ => items.push(json!({"type": "message", "role": "user",
                                   "content": [{"type": "input_text", "text": m["content"].as_str().filter(|s| !s.is_empty()).unwrap_or("(empty)")}]})),
        }
    }
    let ins = if instructions.is_empty() { "You are a helpful coding agent.".to_string() } else { instructions.join("\n\n") };
    (ins, items)
}

/// Talks to the ChatGPT-backed Codex endpoint (Responses API over SSE), authenticated with the stored Codex login.
pub struct CodexResponsesAdapter {
    client: reqwest::Client,
    auth: CodexAuth,
    pub effort: std::sync::Mutex<Option<String>>,
    /// Stable per session: keeps every turn on the backend that holds our prompt cache.
    cache_key: String,
}

impl CodexResponsesAdapter {
    const URL: &'static str = "https://chatgpt.com/backend-api/codex/responses";

    pub fn new(effort: Option<String>) -> CodexResponsesAdapter {
        // Codex streams events steadily, so 60s of silence means the connection is dead: retry now
        CodexResponsesAdapter { client: http_client(60), auth: CodexAuth::default(), effort: std::sync::Mutex::new(effort), cache_key: uuid::Uuid::new_v4().to_string() }
    }

    async fn headers(&self, force: bool) -> Result<Vec<(&'static str, String)>, ProviderError> {
        let (token, account) = self.auth.credentials(force).await.map_err(|e| {
            if e.starts_with("Connection error") { classify(None, &e) } else { ProviderError::new(e, Some(401)) }
        })?;
        Ok(vec![
            ("Authorization", format!("Bearer {token}")),
            ("chatgpt-account-id", account),
            ("OpenAI-Beta", "responses=experimental".into()),
            ("originator", "codex_cli_rs".into()),
            ("session_id", self.cache_key.clone()), // the Codex CLI sends its conversation id here and as prompt_cache_key
            ("Accept", "text/event-stream".into()),
        ])
    }
}

#[async_trait]
impl Adapter for CodexResponsesAdapter {
    fn stream(&self, req: StreamReq) -> EventStream<'_> {
        Box::pin(async_stream::try_stream! {
            // the "codex/" prefix only routes inside melon; the backend wants the bare name
            let (instructions, items) = to_responses_input(&req.messages);
            let mut body = json!({
                "model": req.model.strip_prefix("codex/").unwrap_or(&req.model), "instructions": instructions, "input": items,
                "store": false, "stream": true, "prompt_cache_key": self.cache_key,
            });
            if let Some(tools) = req.tools.as_ref().filter(|t| !t.is_empty()) {
                body["tools"] = json!(tools.iter().map(|t| json!({"type": "function", "name": t["name"],
                    "description": t.get("description").cloned().unwrap_or(json!("")),
                    "parameters": t.get("parameters").cloned().unwrap_or_else(empty_schema)})).collect::<Vec<_>>());
                body["tool_choice"] = json!("auto");
            }
            // no temperature: the Codex backend rejects sampling params
            if let Some(e) = self.effort.lock().unwrap().clone().or(req.effort.clone()) { body["reasoning"] = json!({"effort": e}); }

            let mut headers = self.headers(false).await?;
            let mut resp = None;
            for attempt in 0..2 {
                let mut rb = self.client.post(Self::URL).json(&body);
                for (k, v) in &headers { rb = rb.header(*k, v); }
                let r = rb.send().await.map_err(conn_err)?;
                if r.status().as_u16() == 401 && attempt == 0 {
                    headers = self.headers(true).await?; // token expired mid-session: refresh once and replay
                    continue;
                }
                if r.status().as_u16() != 200 { Err(fail(r).await)?; unreachable!() }
                resp = Some(r);
                break;
            }
            let resp = resp.ok_or_else(|| ProviderError::new("Codex login rejected", Some(401)))?;

            let mut usage: Option<TokenUsage> = None;
            let mut finish = "stop".to_string();
            let mut names: HashMap<String, String> = HashMap::new(); // item id -> call_id, to route argument deltas
            let mut data = sse_data(resp);
            while let Some(payload) = data.next().await {
                let payload = payload?;
                if payload.is_empty() || payload == "[DONE]" { continue; }
                let Ok(ev) = serde_json::from_str::<Value>(&payload) else { continue };
                let kind = ev["type"].as_str().unwrap_or("");
                match kind {
                    "response.output_text.delta" => { yield StreamEvent::TextDelta(ev["delta"].as_str().unwrap_or("").to_string()); }
                    "response.output_item.added" if ev["item"]["type"] == "function_call" => {
                        let it = &ev["item"];
                        names.insert(it["id"].as_str().unwrap_or("").into(), it["call_id"].as_str().unwrap_or("").into());
                        yield StreamEvent::ToolCallStart { call_id: it["call_id"].as_str().unwrap_or("").into(), name: it["name"].as_str().unwrap_or("").into() };
                    }
                    "response.function_call_arguments.delta" => {
                        let id = ev["item_id"].as_str().and_then(|i| names.get(i)).cloned().unwrap_or_default();
                        yield StreamEvent::ToolCallDelta { call_id: id, arguments_delta: ev["delta"].as_str().unwrap_or("").into() };
                    }
                    "response.output_item.done" if ev["item"]["type"] == "function_call" => {
                        let it = &ev["item"];
                        finish = "tool_calls".into();
                        yield StreamEvent::ToolCallComplete(ToolCall {
                            call_id: it["call_id"].as_str().unwrap_or("").into(), name: it["name"].as_str().unwrap_or("").into(),
                            arguments: parse_tool_call_arguments(it["arguments"].as_str().unwrap_or("")),
                        });
                    }
                    "response.completed" | "response.incomplete" => {
                        if kind == "response.incomplete" && ev["response"]["incomplete_details"]["reason"] == "max_output_tokens" {
                            finish = "length".into(); // cut off: the agent must not run a half-written call
                        }
                        let u = &ev["response"]["usage"];
                        usage = Some(TokenUsage {
                            prompt_tokens: u["input_tokens"].as_i64().unwrap_or(0), completion_tokens: u["output_tokens"].as_i64().unwrap_or(0),
                            total_tokens: u["total_tokens"].as_i64().unwrap_or(0), cached_tokens: u["input_tokens_details"]["cached_tokens"].as_i64().unwrap_or(0),
                            cache_write_tokens: 0,
                        });
                    }
                    "response.failed" | "error" => {
                        let err = if !ev["response"]["error"].is_null() { ev["response"]["error"].clone() } else if !ev["error"].is_null() { ev["error"].clone() } else { ev.clone() };
                        let s = err.to_string();
                        Err(classify(None, &format!("Codex error: {}", &s[..s.len().min(300)])))?;
                    }
                    _ => {}
                }
            }
            yield StreamEvent::MessageComplete { finish_reason: Some(finish), usage, reasoning: None, text: None };
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_orphans_patched() {
        let msgs = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": "", "tool_calls": [{"id": "a", "function": {"name": "x", "arguments": "{}"}}]}),
            json!({"role": "user", "content": "next"}),
        ];
        let (sys, out) = to_anthropic_messages(&msgs);
        assert!(sys.is_some());
        // user, assistant(tool_use), user(tool_result placeholder + text)
        assert_eq!(out.len(), 3);
        assert_eq!(out[2]["content"][0]["type"], "tool_result");
        assert_eq!(out[2]["content"][1]["type"], "text");
    }

    #[test]
    fn slots() {
        let mut calls = BTreeMap::new();
        calls.insert(0, ("a".to_string(), "read".to_string(), String::new()));
        assert_eq!(call_slot(&calls, None, "b"), 1);
        assert_eq!(call_slot(&calls, None, ""), 0);
        assert_eq!(call_slot(&calls, None, "a"), 0);
        assert_eq!(call_slot(&calls, Some(3), ""), 3);
    }
}
