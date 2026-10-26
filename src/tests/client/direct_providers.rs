//! Ported from tests/test_direct_providers.py.
use crate::client::adapters::{Adapter, EventStream, StreamReq};
use crate::client::errors::ProviderError;
use crate::client::llm_client::LLMClient;
use crate::client::providers::split;
use crate::client::response::StreamEvent;
use crate::config::{Config, ConfigHandle, ProviderConfig};
use crate::tests::fakes;
use crate::util::trace::Tracer;
use futures::StreamExt;
use std::sync::{Arc, Mutex};

#[test]
fn prefix_picks_a_provider_and_gateway_ids_are_untouched() {
    let cfg = Config::default();
    let (p, wire) = split(&cfg, "ollama:qwen3:8b").unwrap();
    // the tag after the second colon survives
    assert_eq!((p.name.as_str(), wire.as_str(), p.base_url.as_str()), ("ollama", "qwen3:8b", "http://localhost:11434/v1"));
    assert!(split(&cfg, "deepseek/deepseek-v4-flash").is_none()); // a gateway id, as before
    assert!(split(&cfg, "nosuch:model").is_none());
}

#[test]
fn config_adds_a_provider_and_overrides_a_builtin() {
    // a name nothing else in the suite uses, so parallel tests never see it
    let key = "MEW_TEST_WORK_KEY";
    unsafe { std::env::remove_var(key) };
    let mut cfg = Config::default();
    cfg.providers.insert(
        "work".into(),
        ProviderConfig { base_url: Some("https://llm.example.com/v1".into()), api_key_env: Some(key.into()), protocol: Some("anthropic".into()), ..Default::default() },
    );
    cfg.providers.insert("ollama".into(), ProviderConfig { base_url: Some("http://gpu-box:11434/v1".into()), ..Default::default() });
    cfg.providers.insert("broken".into(), ProviderConfig { api_key_env: Some("X".into()), ..Default::default() });

    let (p, _) = split(&cfg, "work:big").unwrap();
    assert_eq!(p.protocol, "anthropic");
    assert_eq!(p.api_key(), None);
    assert!(cfg.missing_key(Some("work:big")).unwrap().contains(key));
    unsafe { std::env::set_var(key, "k") };
    assert_eq!(cfg.missing_key(Some("work:big")), None);
    assert_eq!(split(&cfg, "ollama:x").unwrap().0.base_url, "http://gpu-box:11434/v1");
    assert_eq!(cfg.missing_key(Some("ollama:x")), None); // local servers need no key
    assert!(cfg.validate().iter().any(|e| e.contains("providers.broken")));
}

/// Remembers which model name reached it.
struct Recorder(Arc<Mutex<Option<String>>>);

#[async_trait::async_trait]
impl Adapter for Recorder {
    fn stream(&self, req: StreamReq) -> EventStream<'_> {
        *self.0.lock().unwrap() = Some(req.model);
        let done = StreamEvent::MessageComplete { finish_reason: None, usage: None, reasoning: None, text: None };
        Box::pin(futures::stream::iter(vec![Ok::<_, ProviderError>(done)]))
    }
}

#[tokio::test]
async fn client_sends_the_bare_model_name_to_the_providers_own_endpoint() {
    // The Python test also peeked at the SDK client's host (api.groq.com); the Rust adapter keeps its URL private,
    // so the preset URL is checked through split() instead.
    unsafe { std::env::set_var("GROQ_API_KEY", "gk") };
    fakes::init();
    let mut cfg = Config::default();
    cfg.cwd = std::env::temp_dir();
    cfg.model.name = "groq:llama-3.3-70b".into();
    assert_eq!(split(&cfg, "groq:x").unwrap().0.base_url, "https://api.groq.com/openai/v1");
    let client = Arc::new(LLMClient::new(ConfigHandle::new(cfg), Arc::new(Tracer::new("t", None))));
    let seen = Arc::new(Mutex::new(None));
    client.adapters.lock().unwrap().insert("groq".into(), Arc::new(Recorder(seen.clone())));
    let _: Vec<_> = client.chat_completion(vec![serde_json::json!({"role": "user", "content": "hi"})], None, true, "default").collect().await;
    assert_eq!(seen.lock().unwrap().as_deref(), Some("llama-3.3-70b"));
    assert_eq!(client.provider_of("groq:x"), "groq");
}
