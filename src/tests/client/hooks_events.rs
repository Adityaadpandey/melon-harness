//! Ported from tests/test_hooks_events.py.
//! Stop hooks can keep the agent working; session-start and subagent-stop hooks just run.
use crate::config::{Config, HookConfig, HookTrigger};
use crate::hooks::HookSystem;
use std::path::Path;

fn system(dir: &Path, trigger: HookTrigger, command: &str) -> HookSystem {
    let mut cfg = Config::default();
    cfg.cwd = dir.to_path_buf();
    cfg.hooks_enabled = true;
    cfg.hooks = vec![HookConfig { name: "h".into(), trigger, command: Some(command.into()), script: None, matcher: None, timeout_sec: 30.0, enabled: true }];
    HookSystem::new(&cfg)
}

#[tokio::test]
async fn stop_hook_exit_2_holds_the_agent_back() {
    let dir = tempfile::tempdir().unwrap();
    let hs = system(dir.path(), HookTrigger::Stop, "echo 3 tests still fail >&2; exit 2");
    assert!(hs.trigger_stop("fix it", "done").await.unwrap().contains("3 tests still fail"));
}

#[tokio::test]
async fn stop_hook_exit_0_lets_it_finish() {
    let dir = tempfile::tempdir().unwrap();
    assert!(system(dir.path(), HookTrigger::Stop, "exit 0").trigger_stop("x", "y").await.is_none());
}

#[tokio::test]
async fn session_start_hook_runs_with_its_info() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("seen");
    let hs = system(dir.path(), HookTrigger::SessionStart, &format!("echo \"$AI_AGENT_SESSION_ID\" > {}", out.display()));
    hs.fire(HookTrigger::SessionStart, &[("session_id", "abc".to_string())]).await;
    assert_eq!(std::fs::read_to_string(out).unwrap().trim(), "abc");
}
