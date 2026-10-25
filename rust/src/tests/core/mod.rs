//! Ported from Python tests (see each file's header).
use crate::client::response::{StreamEvent, TokenUsage};
use crate::config::Config;
use serde_json::{json, Value};
use std::path::Path;

mod test_agent_files;
mod test_approval;
mod test_reminders;
mod test_answer_cap;
mod test_ask_user;
mod test_background_shell;
mod test_challenge;
mod test_commands;
mod test_cost;
mod test_precompact;

/// A config rooted in `dir` that approves everything, built the way the loader builds it (so pi-mode defaults apply,
/// like Python's `Config(cwd=..., approval=AUTO)`). `extra` overrides or adds keys, e.g. `json!({"pi_mode": false})`.
pub(crate) fn cfg(dir: &Path, extra: Value) -> Config {
    super::fakes::init();
    let mut map = serde_json::Map::new();
    map.insert("cwd".into(), json!(dir));
    map.insert("approval".into(), json!("auto"));
    if let Some(o) = extra.as_object() {
        for (k, v) in o {
            map.insert(k.clone(), v.clone());
        }
    }
    Config::from_map(map).expect("test config")
}

/// `git init` in `dir` with a committer, so "which files changed" works.
pub(crate) fn git_init(dir: &Path) {
    for a in [&["init", "-q"][..], &["config", "user.email", "a@b"], &["config", "user.name", "t"]] {
        let ok = std::process::Command::new("git").args(a).current_dir(dir).output().unwrap().status.success();
        assert!(ok, "git {a:?} failed");
    }
}

/// Like `fakes::call` but with the call id the test names (two calls in one run need different ids).
pub(crate) fn call_as(name: &str, args: Value, id: &str) -> Vec<StreamEvent> {
    let mut v = super::fakes::call(name, args);
    if let Some(StreamEvent::ToolCallComplete(tc)) = v.first_mut() {
        tc.call_id = id.into();
    }
    v
}

/// What the model says when asked to summarise the history: the three handoff headings the compactor insists on, and
/// the given usage (the summary request itself is what gets billed).
pub(crate) fn summary_turn(text: &str, usage: TokenUsage) -> Vec<StreamEvent> {
    let body = format!("## ORIGINAL GOAL\n{text}\n## COMPLETED ACTIONS\nnone\n## CURRENT STATE\nmid-way\n## NEXT STEP\ncarry on");
    vec![StreamEvent::MessageComplete { finish_reason: None, usage: Some(usage), reasoning: None, text: Some(body) }]
}

/// The tool-result messages (role "tool") of one request, as plain strings.
pub(crate) fn tool_texts(request: &[Value]) -> Vec<String> {
    request.iter().filter(|m| m["role"] == "tool").map(|m| m["content"].as_str().unwrap_or("").to_string()).collect()
}

/// Every text in a request (system excluded), for "does the model see X" checks.
pub(crate) fn all_texts(request: &[Value]) -> Vec<String> {
    request.iter().map(|m| m["content"].as_str().unwrap_or("").to_string()).collect()
}
