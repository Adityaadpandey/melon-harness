//! Ported from tests/test_providers.py.
//! The Python version fed fake SDK chunks into `_consume`. Here the OpenAI adapter streams over HTTP, so those tests
//! talk to a tiny server on 127.0.0.1 that plays back a canned event stream (no real network, no real model).
use crate::client::adapters::{to_anthropic_messages, wire_messages, Adapter, EventStream, OpenAIChatAdapter, StreamReq};
use crate::client::errors::{classify, ProviderError};
use crate::client::llm_client::{sampling, thinking_body, LLMClient};
use crate::client::response::StreamEvent;
use crate::client::routing::{ModelInfo, ModelRegistry, Router};
use crate::config::{Config, ConfigHandle};
use crate::tests::fakes;
use crate::util::trace::Tracer;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ---------------------------------------------------------------- anthropic conversion

#[test]
fn anthropic_conversion_merges_tool_results_and_patches_orphans() {
    let msgs = vec![
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": "do it"}),
        json!({"role": "assistant", "content": "", "tool_calls": [
            {"id": "a", "type": "function", "function": {"name": "x", "arguments": "{\"k\": 1}"}},
            {"id": "b", "type": "function", "function": {"name": "y", "arguments": "{}"}},
        ]}),
        json!({"role": "tool", "tool_call_id": "a", "content": "res-a"}),
        // result for "b" is missing (pruned / crashed mid-turn)
        json!({"role": "user", "content": "carry on"}),
    ];
    let (system, out) = to_anthropic_messages(&msgs);
    assert_eq!(system.unwrap(), vec![json!({"type": "text", "text": "sys"})]);
    let roles: Vec<&str> = out.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "assistant", "user"]);
    assert_eq!(out[1]["content"][0]["input"], json!({"k": 1}));
    let blocks = out[2]["content"].as_array().unwrap();
    let kinds: Vec<&str> = blocks.iter().map(|b| b["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["tool_result", "tool_result", "text"]);
    assert_eq!(blocks[1]["tool_use_id"], "b");
    assert_eq!(blocks[1]["is_error"], true);
}

// ---------------------------------------------------------------- a canned SSE server

/// Serve one canned event stream on localhost and return the base URL to point an adapter at.
/// Each chunk is wrapped as `data: <json>` and the stream ends with `[DONE]`.
async fn serve(chunks: Vec<Value>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        // read the request head and body so the client is not reset while it is still sending
        let mut buf = vec![0u8; 65536];
        let mut got = 0;
        loop {
            let n = sock.read(&mut buf[got..]).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            got += n;
            let text = String::from_utf8_lossy(&buf[..got]).to_string();
            if let Some(split) = text.find("\r\n\r\n") {
                let len = text[..split]
                    .lines()
                    .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                    .unwrap_or(0);
                if got >= split + 4 + len {
                    break;
                }
            }
        }
        let mut body = String::new();
        for c in chunks {
            body.push_str(&format!("data: {c}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");
        let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        let _ = sock.write_all(head.as_bytes()).await;
        let _ = sock.write_all(body.as_bytes()).await;
        let _ = sock.shutdown().await;
    });
    url
}

async fn consume(chunks: Vec<Value>) -> Vec<StreamEvent> {
    let url = serve(chunks).await;
    let ad = OpenAIChatAdapter::new("k", &url, Default::default());
    let req = StreamReq { model: "m".into(), messages: vec![json!({"role": "user", "content": "hi"})], ..Default::default() };
    ad.stream(req).map(|r| r.expect("stream error")).collect().await
}

fn chunk(content: Option<&str>, tcs: Option<Value>, finish: Option<&str>, usage: Option<Value>) -> Value {
    let mut delta = json!({});
    if let Some(c) = content {
        delta["content"] = json!(c);
    }
    if let Some(t) = tcs {
        delta["tool_calls"] = t;
    }
    let mut v = json!({"choices": [{"delta": delta, "finish_reason": finish}]});
    if let Some(u) = usage {
        v["usage"] = u;
    }
    v
}

fn tc(index: Option<i64>, id: Option<&str>, name: Option<&str>, args: &str) -> Value {
    let mut t = json!({"function": {"arguments": args}});
    if let Some(i) = index {
        t["index"] = json!(i);
    }
    if let Some(i) = id {
        t["id"] = json!(i);
    }
    if let Some(n) = name {
        t["function"]["name"] = json!(n);
    }
    t
}

fn completed_calls(evs: &[StreamEvent]) -> Vec<(String, String, Value)> {
    evs.iter()
        .filter_map(|e| match e {
            StreamEvent::ToolCallComplete(c) => Some((c.call_id.clone(), c.name.clone(), Value::Object(c.arguments.clone()))),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------- openai stream parsing

#[tokio::test]
async fn openai_stream_accumulates_arguments_across_chunks() {
    // Regression: arguments after the first chunk used to be dropped.
    let evs = consume(vec![
        chunk(None, Some(json!([tc(Some(0), Some("c1"), Some("write_file"), "{\"pa")])), None, None),
        chunk(None, Some(json!([tc(Some(0), None, None, "th\": \"a.txt\", ")])), None, None),
        chunk(None, Some(json!([tc(Some(0), None, None, "\"content\": \"hi\"}")])), None, None),
        chunk(None, Some(json!([tc(Some(1), Some("c2"), Some("read_file"), "{\"path\": \"b\"}")])), Some("tool_calls"), None),
    ])
    .await;
    assert_eq!(
        completed_calls(&evs),
        vec![
            ("c1".to_string(), "write_file".to_string(), json!({"path": "a.txt", "content": "hi"})),
            ("c2".to_string(), "read_file".to_string(), json!({"path": "b"})),
        ]
    );
}

#[tokio::test]
async fn parallel_calls_without_an_index_stay_separate() {
    // some open-model servers send no index on any delta; they used to merge into one garbled call
    let evs = consume(vec![
        chunk(None, Some(json!([tc(None, Some("c1"), Some("read_file"), "{\"path\": ")])), None, None),
        chunk(None, Some(json!([tc(None, None, None, "\"a\"}")])), None, None),
        chunk(None, Some(json!([tc(None, Some("c2"), Some("grep"), "{\"pattern\": \"x\"}")])), Some("tool_calls"), None),
    ])
    .await;
    assert_eq!(
        completed_calls(&evs),
        vec![
            ("c1".to_string(), "read_file".to_string(), json!({"path": "a"})),
            ("c2".to_string(), "grep".to_string(), json!({"pattern": "x"})),
        ]
    );
}

#[tokio::test]
async fn cache_hits_are_counted_on_every_provider() {
    let usages = [
        json!({"prompt_tokens": 100, "completion_tokens": 5, "total_tokens": 105, "prompt_tokens_details": {"cached_tokens": 80}}),
        json!({"prompt_tokens": 100, "completion_tokens": 5, "total_tokens": 105, "prompt_cache_hit_tokens": 80}), // DeepSeek
        json!({"prompt_tokens": 100, "completion_tokens": 5, "total_tokens": 105, "cached_tokens": 80}),            // Kimi
    ];
    for usage in usages {
        let evs = consume(vec![chunk(Some("hi"), None, Some("stop"), Some(usage.clone()))]).await;
        match evs.last().unwrap() {
            StreamEvent::MessageComplete { usage: Some(u), .. } => assert_eq!(u.cached_tokens, 80, "{usage}"),
            other => panic!("expected the final event to carry usage, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn thinking_is_collected_from_the_stream() {
    let think = |t: &str| json!({"choices": [{"delta": {"reasoning_content": t}, "finish_reason": null}]});
    let evs = consume(vec![think("check the "), think("callers first"), chunk(Some("ok"), None, Some("stop"), None)]).await;
    match evs.last().unwrap() {
        StreamEvent::MessageComplete { reasoning, .. } => assert_eq!(reasoning.as_deref(), Some("check the callers first")),
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------- error classification

#[test]
fn error_classification() {
    // (status, message, retry, fallback)
    let table: Vec<(Option<u16>, &str, bool, bool)> = vec![
        (Some(429), "slow down", true, true),
        (Some(503), "oops", true, true),
        (None, "conn reset", true, true),
        (Some(403), "MODEL_NOT_IN_PLAN: x", false, true),
        (Some(400), "No available providers match", false, true),
        (Some(404), "model not found", false, true),
        (Some(401), "bad key", false, false),
        (Some(400), "invalid tool schema", false, false),
        (Some(400), "Model is overloaded, try again later", true, true), // odd status, transient text
        (Some(422), "upstream timed out", true, true),
        (Some(402), "Payment required", false, true),
        (Some(403), "insufficient balance on this account", false, true),
    ];
    for (status, msg, retry, fallback) in table {
        let e = classify(status, msg);
        assert_eq!((e.retry, e.fallback), (retry, fallback), "{status:?} {msg}");
    }
}

#[test]
fn temperature_rejection_is_recognised() {
    assert!(classify(Some(400), "temperature is not supported").drop_temperature);
}

#[test]
fn plan_wide_usage_limit_is_not_retried_but_another_provider_may_help() {
    let e = classify(Some(429), "You've reached your 5-hour usage limit for your plan. Your limit resets at 19:12");
    assert!(!e.retry && e.fallback && e.quota);
    assert!(classify(Some(429), "slow down").retry); // ordinary burst limits still retry
}

#[test]
fn quota_text_blocks_the_provider_whatever_the_status() {
    assert!(classify(Some(403), "You exceeded your current quota").quota);
}

// ---------------------------------------------------------------- the client, with a scripted adapter

enum Step {
    Fail(ProviderError),
    Say(&'static str),
}

/// Plays back one scripted step per call, per model, and remembers (model, temperature) of every call.
struct FakeAdapter {
    script: Mutex<HashMap<String, VecDeque<Step>>>,
    calls: Mutex<Vec<(String, Option<f64>)>>,
}

impl FakeAdapter {
    fn new(script: Vec<(&str, Vec<Step>)>) -> Arc<FakeAdapter> {
        Arc::new(FakeAdapter { script: Mutex::new(script.into_iter().map(|(m, s)| (m.to_string(), s.into())).collect()), calls: Mutex::new(vec![]) })
    }
    fn models(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|c| c.0.clone()).collect()
    }
}

/// Newtype so the same Arc can sit in the client's adapter table and in the test.
struct Shared<T>(Arc<T>);

#[async_trait::async_trait]
impl Adapter for Shared<FakeAdapter> {
    fn stream(&self, req: StreamReq) -> EventStream<'_> {
        self.0.calls.lock().unwrap().push((req.model.clone(), req.temperature));
        let step = self.0.script.lock().unwrap().get_mut(&req.model).and_then(|q| q.pop_front());
        let items: Vec<Result<StreamEvent, ProviderError>> = match step {
            Some(Step::Fail(e)) => vec![Err(e)],
            Some(Step::Say(t)) => vec![Ok(StreamEvent::TextDelta(t.into())), Ok(done())],
            None => vec![Err(classify(Some(400), "script ran out"))],
        };
        Box::pin(futures::stream::iter(items))
    }
}

fn done() -> StreamEvent {
    StreamEvent::MessageComplete { finish_reason: None, usage: None, reasoning: None, text: None }
}

const CHAT: &str = "/chat/completions";

fn info(id: &str, ctx: usize) -> ModelInfo {
    ModelInfo { id: id.into(), context_length: ctx, endpoints: vec![CHAT.into()], callable: None, tools_ok: None }
}

fn make_client(script: Vec<(&str, Vec<Step>)>, routing: &[(&str, &[&str])]) -> (Arc<LLMClient>, Arc<FakeAdapter>) {
    let mut cfg = Config::default();
    cfg.cwd = std::env::temp_dir();
    cfg.model.name = "m1".into();
    for (role, chain) in routing {
        cfg.routing.insert(role.to_string(), chain.iter().map(|s| s.to_string()).collect());
    }
    fakes::init();
    let c = Arc::new(LLMClient::new(ConfigHandle::new(cfg), Arc::new(Tracer::new("t", None))));
    let fake = FakeAdapter::new(script);
    c.adapters.lock().unwrap().insert(CHAT.into(), Arc::new(Shared(fake.clone())));
    for mid in ["m1", "m2", "m3"] {
        c.registry.put(info(mid, 1_000_000));
    }
    (c, fake)
}

async fn collect(c: &Arc<LLMClient>) -> Vec<StreamEvent> {
    c.chat_completion(vec![json!({"role": "user", "content": "hi"})], None, true, "default").collect().await
}

fn notices(evs: &[StreamEvent]) -> Vec<String> {
    evs.iter().filter_map(|e| if let StreamEvent::Notice(n) = e { Some(n.clone()) } else { None }).collect()
}

fn has_text(evs: &[StreamEvent]) -> bool {
    evs.iter().any(|e| matches!(e, StreamEvent::TextDelta(_)))
}

#[tokio::test]
async fn transient_error_is_retried_on_same_model() {
    // a dropped connection has no status, so the backoff is the short one (about half a second)
    let (c, fake) = make_client(vec![("m1", vec![Step::Fail(classify(None, "conn reset")), Step::Say("ok")])], &[]);
    let evs = collect(&c).await;
    assert_eq!(fake.models(), ["m1", "m1"]);
    assert!(has_text(&evs));
    assert!(notices(&evs).iter().any(|n| n.contains("trying again")));
}

#[tokio::test]
async fn not_in_plan_falls_back_with_notice_and_cools_model() {
    let (c, fake) = make_client(
        vec![("m1", vec![Step::Fail(classify(Some(403), "MODEL_NOT_IN_PLAN"))]), ("m2", vec![Step::Say("from m2")])],
        &[("default", &["m2"])],
    );
    let evs = collect(&c).await;
    let n = notices(&evs);
    assert_eq!(n.len(), 1);
    assert!(n[0].contains("m1") && n[0].contains("m2"));
    assert_eq!(fake.models(), ["m1", "m2"]);
    assert!(c.registry.is_cooling("m1"));
    // next request skips m1 up front instead of paying for another 403
    let (usable, _) = Router::new(c.config.clone()).candidates(&c.registry, "default", false, 0);
    assert_eq!(usable, ["m2"]);
}

#[tokio::test]
async fn permanent_error_does_not_fall_back() {
    let (c, fake) = make_client(
        vec![("m1", vec![Step::Fail(classify(Some(400), "bad schema"))]), ("m2", vec![Step::Say("x")])],
        &[("default", &["m2"])],
    );
    let evs = collect(&c).await;
    assert_eq!(fake.models(), ["m1"]);
    assert!(matches!(evs.last().unwrap(), StreamEvent::Error(_)));
}

#[tokio::test]
async fn temperature_is_dropped_once_then_remembered() {
    let (c, fake) = make_client(vec![("m1", vec![Step::Fail(classify(Some(400), "temperature unsupported")), Step::Say("ok"), Step::Say("ok2")])], &[]);
    c.config.update(|cfg| cfg.model.temperature = Some(0.7));
    collect(&c).await;
    collect(&c).await;
    let dropped: Vec<bool> = fake.calls.lock().unwrap().iter().map(|(_, t)| t.is_none()).collect();
    assert_eq!(dropped, [false, true, true]);
}

#[test]
fn router_skips_models_known_uncallable_or_too_small() {
    let (c, _) = make_client(vec![], &[("review", &["m2", "m3"])]);
    let mut m2 = info("m2", 1_000_000);
    m2.callable = Some(false);
    c.registry.put(m2);
    c.registry.put(info("m3", 10));
    let (usable, skipped) = Router::new(c.config.clone()).candidates(&c.registry, "review", false, 1000);
    // only the user's own model is left: last resort for every role
    assert_eq!(usable, ["m1"]);
    assert_eq!(skipped.len(), 2);
}

#[test]
fn explicit_main_model_is_not_skipped_by_probe() {
    let (c, _) = make_client(vec![], &[]);
    let mut m1 = info("m1", 1_000_000);
    m1.callable = Some(false);
    c.registry.put(m1);
    let (usable, _) = Router::new(c.config.clone()).candidates(&c.registry, "default", false, 0);
    assert_eq!(usable[0], "m1");
}

#[test]
fn model_validation_suggests_close_matches() {
    fakes::init();
    let dir = tempfile::tempdir().unwrap();
    let reg = ModelRegistry::new(ConfigHandle::new(Config::default()), dir.path().to_path_buf());
    for mid in ["gpt-6-luna", "gpt-5.6-luna", "Qwen/Qwen3.8-Max"] {
        reg.put(info(mid, 128_000));
    }
    let (ok, near) = reg.validate("openai/gpt-luna-6");
    assert!(!ok && near.contains(&"gpt-6-luna".to_string()), "{near:?}");
    assert_eq!(reg.validate("Qwen/Qwen3.8-Max"), (true, vec![]));
    let empty = ModelRegistry::new(ConfigHandle::new(Config::default()), dir.path().to_path_buf());
    assert!(empty.validate("anything").0); // offline: don't block
}

#[tokio::test]
async fn unsupported_model_error_suggests_real_ids() {
    let msg = "Model \"m1\" is not supported on this endpoint. code: unsupported_model";
    let (c, _) = make_client(vec![("gpt-luna-6", vec![Step::Fail(classify(Some(400), msg))])], &[]);
    // a catalog that knows the real id but not the typed one
    let dir = tempfile::tempdir().unwrap();
    let reg = ModelRegistry::new(c.config.clone(), dir.path().to_path_buf());
    reg.put(info("gpt-6-luna", 128_000));
    let (_, near) = reg.validate("gpt-luna-6");
    assert!(near.contains(&"gpt-6-luna".to_string()));
    c.registry.put(info("gpt-6-luna", 128_000));
    c.config.update(|cfg| cfg.model.name = "gpt-luna-6".into());
    let evs = collect(&c).await;
    match evs.last().unwrap() {
        StreamEvent::Error(e) => assert!(e.contains("did you mean: gpt-6-luna"), "{e}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn quota_exhausted_gateway_falls_over_to_codex_and_stays_off_the_gateway() {
    let limit = classify(Some(429), "You've reached your 5-hour usage limit for your plan");
    let (c, fake) = make_client(
        vec![("m1", vec![Step::Fail(limit)]), ("codex/x", vec![Step::Say("ok"), Step::Say("ok")])],
        &[("default", &["m2", "codex/x"])],
    );
    c.registry.put(info("codex/x", 1_000_000));
    let evs = collect(&c).await;
    // m2 shares m1's quota, so it is skipped
    assert!(notices(&evs).iter().any(|n| n.contains("Falling back from m1 to codex/x")), "{:?}", notices(&evs));
    assert_eq!(fake.models(), ["m1", "codex/x"]);
    let evs = collect(&c).await; // next request does not touch the gateway again
    assert!(!notices(&evs).iter().any(|n| n.contains("Falling back")));
}

// ---------------------------------------------------------------- output cap

/// Looks like a DeepSeek server: refuses any max_tokens above 8192.
struct CappedFake {
    caps: Mutex<Vec<Option<usize>>>,
}

#[async_trait::async_trait]
impl Adapter for Shared<CappedFake> {
    fn is_openai_chat(&self) -> bool {
        true
    }
    fn stream(&self, req: StreamReq) -> EventStream<'_> {
        self.0.caps.lock().unwrap().push(req.max_tokens);
        let item = match req.max_tokens {
            Some(m) if m > 8192 => Err(classify(Some(400), "Invalid max_tokens value, the valid range of max_tokens is [1, 8192]")),
            _ => Ok(done()),
        };
        Box::pin(futures::stream::iter(vec![item]))
    }
}

#[tokio::test]
async fn output_cap_is_sent_and_a_server_limit_is_learned() {
    let (c, _) = make_client(vec![], &[]);
    let fake = Arc::new(CappedFake { caps: Mutex::new(vec![]) });
    c.adapters.lock().unwrap().insert(CHAT.into(), Arc::new(Shared(fake.clone())));
    collect(&c).await;
    collect(&c).await;
    // refused once, then its own number every time
    assert_eq!(*fake.caps.lock().unwrap(), [Some(32_000), Some(8192), Some(8192)]);
}

#[test]
fn output_cap_shrinks_to_the_room_left() {
    let (c, _) = make_client(vec![], &[]);
    assert_eq!(c.output_cap("m1", 128_000, 110_000), Some(128_000 - 110_000 - 4096));
    assert_eq!(c.output_cap("m1", 1_000_000, 1000), Some(32_000));
}

// ---------------------------------------------------------------- thinking

#[test]
fn thinking_goes_back_only_to_its_own_model_and_the_history_stays_append_only() {
    let msgs = vec![
        json!({"role": "user", "content": "old question"}),
        json!({"role": "assistant", "content": "a", "reasoning_content": "old", "reasoning_model": "deepseek-v4"}),
        json!({"role": "user", "content": "fix it"}),
        json!({"role": "assistant", "content": "", "reasoning_content": "plan", "reasoning_model": "deepseek-v4", "tool_calls": []}),
        json!({"role": "tool", "tool_call_id": "c1", "content": "x"}),
    ];
    let same = wire_messages(&msgs, "deepseek-v4");
    assert_eq!(same[1]["reasoning_content"], "old");
    assert_eq!(same[3]["reasoning_content"], "plan");
    // a harness note (a user message) must not rewrite earlier turns: that would miss the cache from there on
    let mut longer = msgs.clone();
    longer.push(json!({"role": "user", "content": "<system-reminder>x</system-reminder>"}));
    assert_eq!(wire_messages(&longer, "deepseek-v4")[..5], same[..]);
    assert!(!same.iter().any(|m| m.get("reasoning_model").is_some())); // our bookkeeping never reaches the server
    assert!(wire_messages(&msgs, "kimi-k3")[3].get("reasoning_content").is_none()); // another model can't use it
    assert_eq!(msgs[3]["reasoning_model"], "deepseek-v4"); // the stored history is not modified
}

#[test]
fn context_overflow_is_recognised_across_providers() {
    use crate::agent::agent::{not_overflow, overflow};
    let cases = [
        ("This model's maximum context length is 131072 tokens. However, you requested 140000 tokens", true),
        ("the request exceeds the available context size, try increasing it", true), // llama.cpp
        ("Your request exceeded model token limit: 262144 (requested: 270000)", true), // Kimi
        ("Range of input length should be [1, 129024]", true),                        // DashScope / Qwen
        ("ThrottlingException: Too many tokens, please wait before trying again.", false), // a rate limit
    ];
    for (msg, expect) in cases {
        assert_eq!(overflow().is_match(msg) && !not_overflow().is_match(msg), expect, "{msg}");
    }
}

#[test]
fn sampling_follows_the_family_unless_a_temperature_is_set() {
    assert_eq!(sampling("Qwen/Qwen3.8-Max", None), (Some(0.55), Some(1.0)));
    assert_eq!(sampling("moonshotai/Kimi-K3", None), (Some(1.0), Some(0.95)));
    assert_eq!(sampling("deepseek/deepseek-v4-pro", None), (None, None)); // the server's own default
    assert_eq!(sampling("Qwen/Qwen3.8-Max", Some(0.2)), (Some(0.2), None));
}

#[test]
fn thinking_switch_is_worded_for_the_server_it_goes_to() {
    assert_eq!(thinking_body("deepseek", "deepseek-v4-pro", Some("low")), json!({"thinking": {"type": "disabled"}}));
    assert_eq!(thinking_body("zai", "glm-5.3", Some("high")), json!({"thinking": {"type": "enabled", "clear_thinking": false}}));
    assert_eq!(thinking_body("openrouter", "qwen/qwen3.8-max", Some("high")), json!({"reasoning": {"effort": "high"}}));
    assert_eq!(thinking_body("mybox", "Qwen/Qwen3.8-27B", Some("low")), json!({"chat_template_kwargs": {"enable_thinking": false}})); // own vLLM
    assert_eq!(thinking_body("groq", "openai/gpt-oss-120b", Some("high")), json!({"reasoning_effort": "high"}));
    assert_eq!(thinking_body("gateway", "deepseek/deepseek-v4-pro", Some("high")), json!({})); // the gateway ignores every switch
    assert_eq!(thinking_body("deepseek", "deepseek-v4-pro", None), json!({})); // an ordinary request keeps the server default
}

/// A server that has never heard of the thinking switch: any extra body is a 400.
struct ThinkingFake {
    bodies: Mutex<Vec<Option<Value>>>,
}

#[async_trait::async_trait]
impl Adapter for Shared<ThinkingFake> {
    fn is_openai_chat(&self) -> bool {
        true
    }
    fn stream(&self, req: StreamReq) -> EventStream<'_> {
        self.0.bodies.lock().unwrap().push(req.extra_body.clone());
        let item = if req.extra_body.is_some() { Err(classify(Some(400), "Unrecognized request argument supplied: thinking")) } else { Ok(done()) };
        Box::pin(futures::stream::iter(vec![item]))
    }
}

#[tokio::test]
async fn a_server_without_the_switch_is_asked_again_without_it_and_remembered() {
    // Python patched _provider to say "deepseek"; here a deepseek: model id makes the client pick that provider itself
    unsafe { std::env::set_var("DEEPSEEK_API_KEY", "dk") };
    let (c, _) = make_client(vec![], &[]);
    c.config.update(|cfg| cfg.model.name = "deepseek:deepseek-v4-pro".into());
    let fake = Arc::new(ThinkingFake { bodies: Mutex::new(vec![]) });
    c.adapters.lock().unwrap().insert("deepseek".into(), Arc::new(Shared(fake.clone())));
    c.set_auto_effort(Some("high"));
    collect(&c).await;
    collect(&c).await;
    assert_eq!(*fake.bodies.lock().unwrap(), [Some(json!({"thinking": {"type": "enabled"}})), None, None]);
}
