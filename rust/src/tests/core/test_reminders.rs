//! Ported from tests/test_reminders.py: facts that change mid-run reach the model as short reminders after the history.
use super::{all_texts, call_as, summary_turn};
use crate::agent::events::AgentEvent;
use crate::client::response::TokenUsage;
use crate::tests::fakes::{call, run, scripted, text};
use serde_json::json;

fn last_content(req: &[serde_json::Value]) -> String {
    req.last().unwrap()["content"].as_str().unwrap_or("").to_string()
}

#[tokio::test]
async fn a_file_changed_behind_the_agents_back_is_reported_once() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "one\n").unwrap();
    let mut agent = scripted(
        super::cfg(tmp.path(), json!({})),
        vec![
            call("read_file", json!({"path": "a.txt"})),
            call_as("shell", json!({"command": "echo two >> a.txt"}), "c2"), // a formatter or the user would do the same
            call_as("glob", json!({"pattern": "*.txt"}), "c3"),
            text("done"),
        ],
    )
    .await;
    run(&mut agent, "go").await;
    let reqs = agent.session.client.requests.lock().unwrap();
    let third = last_content(&reqs[2]);
    assert!(third.contains("Changed on disk") && third.contains("`a.txt`"), "{third}");
    // not again on the next call
    assert!(!all_texts(&reqs[3][reqs[2].len()..]).iter().any(|c| c.contains("Changed on disk")));
}

#[tokio::test]
async fn a_tool_call_typed_as_text_is_sent_back() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = scripted(
        super::cfg(tmp.path(), json!({})),
        vec![text("I will read it.\n<tool_call>{\"name\": \"read_file\", \"arguments\": {\"path\": \"a.py\"}}</tool_call>"), text("done")],
    )
    .await;
    run(&mut agent, "go").await;
    assert!(last_content(&agent.session.client.requests.lock().unwrap()[1]).contains("tool call as text"));
}

#[tokio::test]
async fn an_untouched_task_list_is_brought_back() {
    let tmp = tempfile::tempdir().unwrap();
    let plan = call("todos", json!({"action": "set", "items": [{"content": "read the code"}, {"content": "fix it"}, {"content": "test it"}]}));
    let mut script = vec![plan];
    script.extend((0..11).map(|i| call_as("glob", json!({"pattern": format!("*{i}.py")}), &format!("g{i}"))));
    script.push(text("done"));
    let mut agent = scripted(super::cfg(tmp.path(), json!({})), script).await;
    run(&mut agent, "go").await;
    let reqs = agent.session.client.requests.lock().unwrap();
    let sent = all_texts(reqs.last().unwrap());
    assert!(sent.iter().any(|c| c.contains("task list has not been touched") && c.contains("fix it")));
}

#[tokio::test]
async fn compaction_stops_when_the_context_refills_right_away() {
    let tmp = tempfile::tempdir().unwrap();
    // keep_recent_tokens 1: even this short history has an older part to fold, so each refill really compacts
    let cfg = super::cfg(tmp.path(), json!({"pi_mode": false, "compact_at_tokens": 1, "keep_recent_tokens": 1}));
    let sum = || summary_turn("goal", TokenUsage { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2, ..Default::default() });
    let glob = |i: usize| call_as("glob", json!({"pattern": format!("*{i}")}), &format!("g{i}"));
    // every compaction asks the model for its summary first, so a summary turn sits before each call that follows one
    let script = vec![glob(0), sum(), glob(1), sum(), glob(2), sum(), glob(3), glob(4), glob(5), glob(6), text("done")];
    let mut agent = scripted(cfg, script).await;
    let events = run(&mut agent, "go").await;
    let notes: Vec<&String> = events.iter().filter_map(|e| if let AgentEvent::Notice(m) = e { Some(m) } else { None }).collect();
    let compactions = notes.iter().filter(|m| m.contains("Context compacted")).count();
    assert_eq!(compactions, 3); // the fourth refill in a row prunes instead of paying for another summary
    assert!(notes.iter().any(|m| m.contains("fills up again")), "{notes:?}");
}
