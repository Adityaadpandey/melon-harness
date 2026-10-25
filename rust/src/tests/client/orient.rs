//! Ported from tests/test_orient.py.
use crate::agent::events::AgentEvent;
use crate::agent::orient::{orient, parse, repo_map};
use crate::client::response::StreamEvent;
use crate::config::ApprovalPolicy;
use crate::tests::fakes::*;
use serde_json::json;

#[test]
fn the_reply_is_read_into_an_orientation_and_junk_is_ignored() {
    let got = parse(r#"sure {"intent": "review", "tier": "default", "where": ["apps/server/src/otp"], "queries": ["otp expiry"], "note": "the uncommitted change"}"#).unwrap();
    assert_eq!(got.intent, "review");
    assert_eq!(got.where_, ["apps/server/src/otp"]);
    assert!(got.hint().contains("otp expiry"));
    assert!(parse("no json here").is_none());
    assert_eq!(parse(r#"{"intent": "banana", "tier": "huge"}"#).unwrap().intent, "other");
}

#[test]
fn repo_map_shows_folders_recent_and_uncommitted_work() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(d).status().unwrap().success());
    std::fs::create_dir_all(d.join("apps/web/src")).unwrap();
    std::fs::write(d.join("apps/web/src/a.ts"), "x").unwrap();
    std::fs::write(d.join("README.md"), "hi").unwrap();
    let m = repo_map(d, &[]);
    assert!(m.contains("apps/web") && m.contains("README.md") && m.contains("UNCOMMITTED") && m.contains("a.ts"), "{m}");
}

fn reply(json_text: &str) -> Vec<StreamEvent> {
    vec![StreamEvent::MessageComplete { finish_reason: None, usage: None, reasoning: None, text: Some(json_text.into()) }]
}

#[tokio::test]
async fn a_failed_call_means_no_orientation() {
    let dir = tempfile::tempdir().unwrap();
    let a = scripted(config(dir.path()), vec![error("boom")]).await;
    assert!(orient(&a.session.client, "fast", "review the change i have", "FOLDERS: x").await.is_none());
}

#[tokio::test]
async fn the_fast_model_answer_is_used() {
    let dir = tempfile::tempdir().unwrap();
    let a = scripted(config(dir.path()), vec![reply(r#"{"intent": "review", "tier": "fast"}"#)]).await;
    let got = orient(&a.session.client, "fast", "review the change i have", "FOLDERS: x").await.unwrap();
    assert_eq!((got.intent.as_str(), got.tier), ("review", "fast"));
}

#[test]
fn what_the_change_should_land_in_is_read_from_the_reply() {
    assert_eq!(parse(r#"{"intent": "change", "touches": "code"}"#).unwrap().touches, "code");
    assert_eq!(parse(r#"{"intent": "change", "touches": "poetry"}"#).unwrap().touches, "none");
}

#[tokio::test]
async fn a_bug_fix_that_only_edited_a_doc_is_sent_back() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(d).status().unwrap().success());
    let mut cfg = config(d);
    cfg.approval = ApprovalPolicy::Auto;
    cfg.auto_verify = false;
    cfg.check_references = false;
    // Python replaced Agent._orient with a stub. Here orientation is a real call to the fast model, so the first scripted
    // reply is its answer ("this is a code change"); a fast route must exist for the agent to ask at all.
    cfg.routing.insert("fast".into(), vec!["flash".into()]);
    let turns = vec![
        reply(r#"{"intent": "change", "tier": "default", "touches": "code"}"#),
        call("write_file", json!({"path": "NOTES.md", "content": "n"})),
        text("noted"),
        call("write_file", json!({"path": "fix.ts", "content": "x"})),
        text("fixed"),
    ];
    let mut a = scripted(cfg, turns).await;
    let evs = run(&mut a, "the invite link does not open, fix it please").await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice(m) if m.contains("Only documentation changed"))));
    assert!(d.join("fix.ts").exists());
}
