//! Ported from tests/test_codex.py.
//! Not ported: test_stream_parsing (the Rust Codex adapter parses events inline while streaming from the fixed
//! chatgpt.com URL, so there is no offline seam), test_codex_luna_is_the_default_model (the Rust default model is
//! MiniMax-M3 now), test_reasoning_effort_reaches_the_codex_adapter (adapters are only reachable as trait objects).
use crate::client::adapters::{to_responses_input, CodexResponsesAdapter};
use crate::client::routing::ModelRegistry;
use crate::config::{Config, ConfigHandle};
use crate::tests::fakes;
use base64::Engine;
use serde_json::json;
use std::sync::OnceLock;

#[test]
fn history_conversion() {
    let msgs = vec![
        json!({"role": "system", "content": "be brief"}),
        json!({"role": "user", "content": "hi"}),
        json!({"role": "assistant", "content": "", "tool_calls": [{"id": "c1", "function": {"name": "read", "arguments": "{\"p\":1}"}}]}),
        json!({"role": "tool", "tool_call_id": "c1", "content": "data"}),
    ];
    let (instr, items) = to_responses_input(&msgs);
    assert_eq!(instr, "be brief");
    let kinds: Vec<&str> = items.iter().map(|i| i["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["message", "function_call", "function_call_output"]);
    assert_eq!(items[1]["call_id"], "c1");
    assert_eq!(items[2]["call_id"], "c1");
}

#[test]
fn codex_models_route_to_responses() {
    fakes::init();
    let dir = tempfile::tempdir().unwrap();
    let reg = ModelRegistry::new(ConfigHandle::new(Config::default()), dir.path().to_path_buf());
    assert_eq!(reg.get("codex/gpt-5.5").endpoint(), "/responses");
}

/// A Codex home with a login whose token is good for a very long time, so no refresh is ever attempted.
fn codex_home() -> &'static std::path::Path {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let enc = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        let jwt = format!("{}.{}.sig", enc("{}"), enc("{\"exp\": 99999999999}"));
        std::fs::write(dir.path().join("auth.json"), json!({"tokens": {"access_token": jwt, "account_id": "acct"}}).to_string()).unwrap();
        unsafe { std::env::set_var("CODEX_HOME", dir.path()) };
        dir
    })
    .path()
}

#[tokio::test]
async fn codex_requests_carry_a_stable_cache_session() {
    codex_home();
    let a = CodexResponsesAdapter::new(None);
    let h1 = a.headers(false).await.unwrap();
    let h2 = a.headers(false).await.unwrap();
    let session = |h: &Vec<(&'static str, String)>| h.iter().find(|(k, _)| *k == "session_id").unwrap().1.clone();
    // without this header the backend reports 0 cached tokens on every turn
    assert_eq!(session(&h1), session(&h2));
    assert!(session(&h1).len() >= 32);
    let other = CodexResponsesAdapter::new(None).headers(false).await.unwrap();
    assert_ne!(session(&other), session(&h1));
}
