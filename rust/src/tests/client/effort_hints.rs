//! Ported from tests/test_effort_hints.py.
use crate::agent::routing_policy::{classify, effort, length_hint};
use crate::client::adapters::{Adapter, EventStream, StreamReq};
use crate::client::errors::ProviderError;
use crate::client::llm_client::LLMClient;
use crate::client::response::StreamEvent;
use crate::config::{Config, ConfigHandle};
use crate::tests::fakes;
use crate::util::trace::Tracer;
use futures::StreamExt;
use std::sync::{Arc, Mutex};

#[test]
fn difficulty_sets_effort_and_length() {
    assert_eq!(effort(classify("rename foo to bar in utils.py").0), Some("low"));
    assert_eq!(effort(classify("debug the flaky webhook retry").0), Some("high"));
    assert!(length_hint("review my change", "default", false).starts_with("Answer with at most 6 findings"));
    assert!(length_hint("explain how the router works", "default", false).contains("300 words"));
    assert!(length_hint("rename foo to bar", "fast", true).contains("1-2 sentences"));
    assert_eq!(length_hint("debug the race in the queue", "strong", true), "");
}

/// Stands in for the Codex adapter and notes the effort each request carried.
struct Codex(Arc<Mutex<Vec<Option<String>>>>);

#[async_trait::async_trait]
impl Adapter for Codex {
    fn stream(&self, req: StreamReq) -> EventStream<'_> {
        self.0.lock().unwrap().push(req.effort.clone());
        let done = StreamEvent::MessageComplete { finish_reason: None, usage: None, reasoning: None, text: None };
        Box::pin(futures::stream::iter(vec![Ok::<_, ProviderError>(done)]))
    }
}

#[tokio::test]
async fn summaries_think_low_and_an_explicit_effort_wins() {
    fakes::init();
    let seen = Arc::new(Mutex::new(vec![]));
    let run = |cfg_effort: Option<&str>, role: &'static str| {
        let seen = seen.clone();
        let cfg_effort = cfg_effort.map(String::from);
        async move {
            let mut cfg = Config::default();
            cfg.cwd = std::env::temp_dir();
            cfg.model.name = "codex/x".into();
            cfg.reasoning_effort = cfg_effort;
            let c = Arc::new(LLMClient::new(ConfigHandle::new(cfg), Arc::new(Tracer::new("t", None))));
            c.set_auto_effort(Some("high"));
            c.adapters.lock().unwrap().insert("/responses".into(), Arc::new(Codex(seen)));
            let _: Vec<_> = c.chat_completion(vec![serde_json::json!({"role": "user", "content": "hi"})], None, true, role).collect().await;
        }
    };
    run(None, "default").await;
    run(Some("low"), "default").await;
    run(Some("high"), "summarize").await;
    let got: Vec<Option<String>> = seen.lock().unwrap().clone();
    assert_eq!(got, [Some("high".to_string()), Some("low".to_string()), Some("low".to_string())]);
}
