//! Shared helpers for the ported test-suite: scripted model replies and a throwaway data folder.
//! Mirrors tests/fakes.py + tests/conftest.py from the Python version.
use crate::agent::agent::Agent;
use crate::agent::events::AgentEvent;
use crate::client::response::{StreamEvent, TokenUsage, ToolCall};
use crate::config::Config;
use serde_json::{Map, Value};
use std::sync::{Once, OnceLock};
use std::path::PathBuf;

static INIT: Once = Once::new();
static DATA: OnceLock<tempfile::TempDir> = OnceLock::new();

/// Point melon's data folder (sessions, traces, lessons, model catalog) at a temp dir, once per test process.
pub fn init() -> PathBuf {
    let dir = DATA.get_or_init(|| tempfile::tempdir().unwrap());
    INIT.call_once(|| {
        std::fs::write(dir.path().join("models.json"), "[]").ok();
        unsafe { std::env::set_var("MELON_DATA_DIR", dir.path()) };
    });
    dir.path().to_path_buf()
}

fn done() -> StreamEvent {
    StreamEvent::MessageComplete { finish_reason: None, usage: Some(TokenUsage { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2, ..Default::default() }), reasoning: None, text: None }
}

pub fn text(s: &str) -> Vec<StreamEvent> {
    vec![StreamEvent::TextDelta(s.into()), done()]
}

pub fn args(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

pub fn call(name: &str, a: Value) -> Vec<StreamEvent> {
    vec![StreamEvent::ToolCallComplete(ToolCall { call_id: "c1".into(), name: name.into(), arguments: args(a) }), done()]
}

pub fn calls(pairs: Vec<(&str, Value)>) -> Vec<StreamEvent> {
    let mut v: Vec<StreamEvent> = pairs
        .into_iter()
        .enumerate()
        .map(|(i, (n, a))| StreamEvent::ToolCallComplete(ToolCall { call_id: format!("c{i}"), name: n.into(), arguments: args(a) }))
        .collect();
    v.push(StreamEvent::MessageComplete { finish_reason: None, usage: None, reasoning: None, text: None });
    v
}

pub fn error(msg: &str) -> Vec<StreamEvent> {
    vec![StreamEvent::Error(msg.into())]
}

/// A config rooted in `cwd`, auto-approving, with no personal state leaking in.
pub fn config(cwd: &std::path::Path) -> Config {
    init();
    let mut c = Config::default();
    c.cwd = cwd.to_path_buf();
    c
}

/// An agent whose model replies come from `turns`, one per request.
pub async fn scripted(cfg: Config, turns: Vec<Vec<StreamEvent>>) -> Agent {
    init();
    let agent = Agent::new(cfg, None).await;
    *agent.session.client.script.lock().unwrap() = Some(turns.into());
    agent
}

/// Run one request and collect every event it produced.
pub async fn run(agent: &mut Agent, msg: &str) -> Vec<AgentEvent> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent.run(msg, tx).await;
    let mut out = vec![];
    while let Ok(e) = rx.try_recv() {
        out.push(e);
    }
    out
}

/// The final answer text of a run, if it ended normally.
pub fn answer(events: &[AgentEvent]) -> Option<String> {
    events.iter().rev().find_map(|e| match e {
        AgentEvent::AgentEnd { response, .. } => response.clone(),
        _ => None,
    })
}
