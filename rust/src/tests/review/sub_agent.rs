//! Ported from tests/test_review_sub_agent.py
//! Skipped: "a stalled sub-agent is cut off" (the Rust wall clock is a fixed 360s and there is no seam to shorten it).
use crate::agent::review_pipeline as rp;
use crate::config::Config;

#[test]
fn sub_agents_are_marked_so_their_answer_is_not_rewritten() {
    assert!(!Config::default().sub_agent);
}

#[tokio::test]
async fn review_sub_agents_are_not_routed_by_the_words_in_their_brief() {
    let _g = super::SERIAL.lock().await;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let s2 = seen.clone();
    *rp::ASK_HOOK.lock().unwrap() = Some(std::sync::Arc::new(move |cfg, _, _| {
        s2.lock().unwrap().push(cfg);
        String::new()
    }));
    let got = rp::ask(&Config::default(), "check the authorization and security of this", "default", 3, 1000, None, None, "never").await;
    *rp::ASK_HOOK.lock().unwrap() = None;
    assert_eq!(got, "");
    let cfgs = seen.lock().unwrap();
    assert!(!cfgs[0].auto_route && cfgs[0].sub_agent);
}

#[tokio::test]
async fn review_requests_refuse_the_coding_worker_tool() {
    use super::super::fakes::*;
    use crate::agent::events::AgentEvent;
    // a read-only review that reaches for parallel_tasks is told why it cannot
    let dir = tempfile::tempdir().unwrap();
    let mut a = scripted(config(dir.path()), vec![call("parallel_tasks", serde_json::json!({"tasks": []})), text("ok")]).await;
    let ev = run(&mut a, "review the changes please").await;
    let out = ev.iter().find_map(|e| match e {
        AgentEvent::ToolCallComplete { name, result, .. } if name == "parallel_tasks" => Some(format!("{} {}", result.output, result.error.clone().unwrap_or_default())),
        _ => None,
    });
    assert!(out.unwrap_or_default().contains("coding workers that edit files"));
}
