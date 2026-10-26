//! Ported from tests/test_answer_cap.py: a final answer far over the limit is not rewritten (the user already read it),
//! but the next request carries a reminder. Also covers the stop hook, which lived in the same Python file.
use crate::agent::brevity::LONG_ANSWER_WORDS;
use crate::agent::review::ReviewResult;
use crate::config::{HookConfig, HookTrigger};
use crate::tests::fakes::{run, scripted, text};
use serde_json::json;

#[test]
fn limit_is_well_above_the_prompt_rule() {
    assert!(LONG_ANSWER_WORDS > 120); // the prompt asks for 120; only a clear overrun triggers the reminder
}

#[test]
fn review_result_counts_blocked_calls() {
    let mut r = ReviewResult { verdict: "PASS".into(), findings: String::new(), blocked: 0 };
    assert_eq!(r.blocked, 0);
    r.blocked = 2;
    assert_eq!(r.blocked, 2);
}

#[tokio::test]
async fn long_answer_earns_a_reminder_next_request() {
    let tmp = tempfile::tempdir().unwrap();
    super::git_init(tmp.path());
    let cfg = super::cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false, "check_references": false}));
    let mut a = scripted(cfg, vec![text(&"word ".repeat(400)), text("ok")]).await;
    run(&mut a, "explain it").await;
    run(&mut a, "and again").await;
    let reqs = a.session.client.requests.lock().unwrap();
    let last = reqs[1].last().unwrap()["content"].as_str().unwrap().to_string();
    assert!(last.contains("ran 400 words"), "{last}");
}

#[tokio::test]
async fn stop_hook_sends_the_work_back_once_then_lets_go() {
    let tmp = tempfile::tempdir().unwrap();
    super::git_init(tmp.path());
    let hook = HookConfig {
        name: "tests".into(),
        trigger: HookTrigger::Stop,
        command: Some("echo 2 tests fail >&2; exit 2".into()),
        script: None,
        matcher: None,
        timeout_sec: 30.0,
        enabled: true,
    };
    let mut cfg = super::cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false, "check_references": false, "hooks_enabled": true}));
    cfg.hooks = vec![hook];
    let mut a = scripted(cfg, vec![text("done"), text("done again"), text("final")]).await;
    let evs = run(&mut a, "hi").await;
    let stops = evs.iter().filter(|e| matches!(e, crate::agent::events::AgentEvent::Notice(m) if m.contains("stop hook"))).count();
    assert_eq!(stops, 2); // capped at two
    let reqs = a.session.client.requests.lock().unwrap();
    assert!(reqs[1].iter().any(|m| m["content"].as_str().map_or(false, |c| c.contains("2 tests fail"))));
}
