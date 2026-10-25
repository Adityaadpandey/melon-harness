//! Ported from tests/test_routing_policy.py.
//! The Python spy recorded the role of every chat_completion call. Rust has no such hook, so the request's tier is
//! read from the "route" trace event (absent = the default tier) and the escalation from the agent's last role.
use crate::agent::routing_policy::{classify, keep_cached_model};
use crate::agent::agent::Agent;
use crate::agent::events::AgentEvent;
use crate::client::llm_client::backoff;
use crate::config::{ApprovalPolicy, Config};
use crate::tests::fakes::*;

#[test]
fn classifier() {
    let long = "x".repeat(800);
    let cases: Vec<(&str, &str)> = vec![
        ("rename the helper getUser to fetchUser", "fast"),
        ("where is the discount code validated?", "fast"),
        ("fix the race condition in booking expiry", "strong"),
        ("investigate why payments fail intermittently", "strong"),
        ("refactor the cart module", "strong"),
        ("update apps/server/src/a.ts, apps/server/src/b.ts, lib/c.ts and lib/d.ts to use the new type", "strong"),
        ("add a loading spinner to the events page", "default"),
        (&long, "strong"),
    ];
    for (req, tier) in cases {
        assert_eq!(classify(req).0, tier, "{req}");
    }
}

fn routes() -> std::collections::BTreeMap<String, Vec<String>> {
    [("fast".to_string(), vec!["flash".to_string()]), ("strong".to_string(), vec!["big".to_string()])].into()
}

fn base(cwd: &std::path::Path) -> Config {
    let mut c = config(cwd);
    c.approval = ApprovalPolicy::Auto;
    c.auto_verify = false;
    c.check_references = false;
    c.auto_orient = false;
    c
}

/// Run one request on a fresh agent; return the tier it was routed to and the events.
async fn run_tier(cfg: Config, turns: Vec<Vec<crate::client::response::StreamEvent>>, msg: &str) -> (String, Vec<AgentEvent>, Agent) {
    let mut a = scripted(cfg, turns).await;
    let evs = run(&mut a, msg).await;
    let tier = a
        .session
        .tracer
        .snapshot()
        .iter()
        .rev()
        .find(|e| e["event"] == "route")
        .map(|e| e["tier"].as_str().unwrap().to_string())
        .unwrap_or_else(|| "default".into());
    (tier, evs, a)
}

#[tokio::test]
async fn each_request_uses_the_tier_its_difficulty_calls_for() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = base(dir.path());
    cfg.routing = routes();
    let (tier, evs, _) = run_tier(cfg.clone(), vec![text("ok")], "rename the helper getUser to fetchUser").await;
    assert_eq!(tier, "fast");
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice(m) if m.contains("fast model"))));
    assert_eq!(run_tier(cfg.clone(), vec![text("ok")], "debug the intermittent failure").await.0, "strong");
    assert_eq!(run_tier(cfg, vec![text("ok")], "add a spinner to the page").await.0, "default");
}

#[tokio::test]
async fn pinned_model_or_missing_chains_disable_routing() {
    let dir = tempfile::tempdir().unwrap();
    let mut pinned = base(dir.path());
    pinned.routing = routes();
    pinned.model_pinned = true;
    assert_eq!(run_tier(pinned, vec![text("ok")], "rename x to y").await.0, "default");
    let no_chains = base(dir.path());
    assert_eq!(run_tier(no_chains, vec![text("ok")], "rename x to y").await.0, "default");
}

#[tokio::test]
async fn failed_verification_escalates_to_the_strong_model() {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git").args(["init", "-q"]).current_dir(dir.path()).status().unwrap();
    let mut cfg = base(dir.path());
    cfg.routing = routes();
    cfg.auto_verify = true;
    cfg.verify_commands = vec!["test -f ok.txt".into()];
    let turns = vec![
        call("write_file", serde_json::json!({"path": "bad.txt", "content": "1"})),
        text("done"),
        call("write_file", serde_json::json!({"path": "ok.txt", "content": "1"})),
        text("fixed"),
    ];
    let (tier, _, a) = run_tier(cfg, turns, "rename x to y").await;
    // starts cheap, escalates once a repair is needed
    assert_eq!(tier, "fast");
    assert_eq!(a.last_role, "strong");
}

#[test]
fn a_big_cached_context_stays_on_its_model_unless_the_request_is_ordinary_anyway() {
    assert_eq!(keep_cached_model("strong", 90_000), "default");
    assert_eq!(keep_cached_model("fast", 25_000), "default");
    assert_eq!(keep_cached_model("strong", 3_000), "strong"); // a small context loses nothing by switching
    assert_eq!(keep_cached_model("default", 90_000), "default");
}

#[test]
fn a_busy_upstream_is_waited_for_longer_than_a_dropped_connection() {
    assert!(backoff(0, Some(429)) >= 2.4 && backoff(2, Some(503)) >= 16.0);
    assert!(backoff(0, None) <= 1.0);
}
