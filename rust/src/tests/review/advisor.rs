//! Ported from tests/test_advisor.py
use crate::config::{Config, ConfigHandle};
use crate::hooks::HookSystem;
use crate::safety::approval::ApprovalManager;
use crate::tests::fakes::*;
use crate::tools::builtin::advisor::{render_transcript, AdvisorTool};
use crate::tools::registry::create_default_registry;
use crate::tools::{Tool, ToolInvocation, ToolResult};
use serde_json::{json, Value};
use std::sync::Arc;

fn from_map(v: Value) -> Config {
    init();
    Config::from_map(v.as_object().cloned().unwrap()).unwrap()
}

async fn consult(tool: &AdvisorTool, cwd: &std::path::Path) -> ToolResult {
    tool.execute(&ToolInvocation { params: json!({}), cwd: cwd.to_path_buf(), epoch: 0, call_id: "c".into(), progress: None }).await
}

/// An advisor wired to a scripted model and a fixed transcript.
async fn advisor(dir: &std::path::Path, cfg: Config, turns: Vec<Vec<crate::client::response::StreamEvent>>, transcript: Vec<Value>) -> (AdvisorTool, Arc<crate::client::llm_client::LLMClient>) {
    let agent = scripted(cfg.clone(), turns).await;
    let client = agent.session.client.clone();
    let tool = AdvisorTool::new(ConfigHandle::new(cfg));
    tool.forced.store(true, std::sync::atomic::Ordering::SeqCst); // these tests use tiny transcripts
    *tool.client.lock().unwrap() = Some(client.clone());
    *tool.transcript.lock().unwrap() = Some(Arc::new(move || transcript.clone()));
    let _ = dir;
    (tool, client)
}

#[test]
fn the_transcript_keeps_the_request_and_drops_the_oldest_middle_when_it_is_too_long() {
    let mut msgs = vec![json!({"role": "system", "content": "sys"}), json!({"role": "user", "content": "fix the login bug"})];
    msgs.extend((0..100).map(|n| json!({"role": "tool", "content": format!("result {n} {}", "x".repeat(1000))})));
    let text = render_transcript(&msgs, 5000);
    assert!(text.contains("fix the login bug") && !text.contains("sys") && text.contains("[... earlier steps left out ...]") && text.contains("result 99") && !text.contains("result 0 "));
}

#[tokio::test]
async fn the_advisor_forwards_the_whole_conversation_and_returns_its_advice() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = vec![json!({"role": "user", "content": "task"}), json!({"role": "assistant", "content": "plan", "tool_calls": [{"function": {"name": "edit", "arguments": "{}"}}]})];
    let (tool, client) = advisor(dir.path(), config(dir.path()), vec![text("1. the cause is the shared helper")], transcript).await;
    let out = consult(&tool, dir.path()).await;
    assert!(out.success && out.output.starts_with("ADVISOR:") && out.output.contains("shared helper"), "{out:?}");
    // the Python test also checks the request went to the "advisor" role; the request log keeps messages only
    let sent = client.requests.lock().unwrap()[0].clone();
    let body = sent[1]["content"].as_str().unwrap();
    assert!(body.contains("[user] task") && body.contains("-> edit("));
}

#[tokio::test]
async fn the_advisor_never_blocks_the_work_when_it_cannot_answer_and_stops_at_its_call_limit() {
    let dir = tempfile::tempdir().unwrap();
    let loose = AdvisorTool::new(ConfigHandle::new(config(dir.path())));
    assert!(consult(&loose, dir.path()).await.error.unwrap().contains("not connected"));

    let (bad, _) = advisor(dir.path(), config(dir.path()), vec![error("quota"), error("quota")], vec![]).await;
    let err = consult(&bad, dir.path()).await.error.unwrap();
    assert!(err.contains("could not answer") && err.contains("Carry on"));

    let mut one = config(dir.path());
    one.advisor_max_calls = 1;
    let (ok, _) = advisor(dir.path(), one, vec![text("1. fine"), text("1. fine")], vec![]).await;
    assert!(consult(&ok, dir.path()).await.success);
    assert!(consult(&ok, dir.path()).await.error.unwrap().contains("limit reached"));
}

#[test]
fn the_advisor_is_a_default_tool_but_a_sub_agent_does_not_get_it() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_string_lossy().to_string();
    let has = |v: Value| from_map(v).allowed_tools.unwrap_or_default().contains(&"advisor".to_string());
    assert!(has(json!({"cwd": cwd})));
    assert!(!has(json!({"cwd": cwd, "model_role": "review"})));
    assert!(create_default_registry(ConfigHandle::new(from_map(json!({"cwd": cwd})))).get("advisor").is_some());
    assert!(!has(json!({"cwd": cwd, "advisor": false})));
}

#[tokio::test]
async fn a_gate_refuses_a_call_with_its_message_and_lets_everything_else_through() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let reg = create_default_registry(ConfigHandle::new(cfg.clone()));
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    *reg.gate.lock().unwrap() = Some(Arc::new(|name: &str| (name == "edit").then(|| "Consult the advisor first.".to_string())));
    let approval = ApprovalManager::new(cfg.approval, dir.path(), &[]);
    let blocked = reg.invoke("edit", json!({"path": "a.txt", "old_string": "x", "new_string": "y"}), dir.path(), &HookSystem::new(&cfg), Some(&approval), "c1").await;
    let msg = format!("{} {}", blocked.output, blocked.error.clone().unwrap_or_default());
    assert!(!blocked.success && msg.contains("Consult the advisor first."));
    assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "x\n");
}

#[test]
fn the_advisor_refuses_a_small_or_vague_task_and_accepts_a_serious_one() {
    use crate::tools::builtin::advisor::is_serious;
    let read = |p: &str| json!({"role": "assistant", "tool_calls": [{"function": {"name": "read_file", "arguments": json!({"path": p}).to_string()}}]});
    let one_file: Vec<Value> = std::iter::once(json!({"role": "user", "content": "make it better"})).chain((0..8).map(|_| read("a.html"))).collect();
    assert!(!is_serious(&one_file));
    let wide: Vec<Value> = (0..6).map(|i| read(&format!("f{i}.py"))).collect();
    assert!(is_serious(&wide));
    assert!(is_serious(&vec![json!({"role": "tool", "content": "Error: boom"}); 3]));
}
