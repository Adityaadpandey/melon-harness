//! Ported from tests/test_permission_flow.py.
//! A deny rule stops the call before anything runs; an allow rule skips the prompt but not the safety refusals.
use crate::config::{ApprovalPolicy, Config, ConfigHandle};
use crate::hooks::HookSystem;
use crate::safety::approval::ApprovalManager;
use crate::safety::rules::save_rule;
use crate::tests::fakes;
use crate::tools::registry::create_default_registry;
use crate::tools::ToolResult;
use serde_json::{json, Value};
use std::path::Path;

async fn run(dir: &Path, tool: &str, params: Value, policy: ApprovalPolicy) -> ToolResult {
    fakes::init();
    let mut cfg = Config::default();
    cfg.cwd = dir.to_path_buf();
    cfg.sandbox = "off".into();
    let reg = create_default_registry(ConfigHandle::new(cfg.clone()));
    let mgr = ApprovalManager::new(policy, dir, &[]); // no callback: anything that would prompt fails closed
    reg.invoke(tool, params, dir, &HookSystem::new(&cfg), Some(&mgr), "c1").await
}

#[tokio::test]
async fn deny_blocks_a_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "hi").unwrap();
    save_rule(dir.path(), "deny", "Read(notes*)").unwrap();
    let r = run(dir.path(), "read_file", json!({"path": "notes.txt"}), ApprovalPolicy::OnRequest).await;
    assert!(!r.success && r.error.unwrap().contains("permission rule"));
}

#[tokio::test]
async fn allow_skips_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    // no one to ask: fails closed
    assert!(!run(dir.path(), "shell", json!({"command": "touch a.txt"}), ApprovalPolicy::OnRequest).await.success);
    save_rule(dir.path(), "allow", "Bash(touch:*)").unwrap();
    let r = run(dir.path(), "shell", json!({"command": "touch a.txt"}), ApprovalPolicy::OnRequest).await;
    assert!(r.success && dir.path().join("a.txt").exists(), "{:?}", r.error);
}

#[tokio::test]
async fn allow_does_not_lift_plan_mode() {
    let dir = tempfile::tempdir().unwrap();
    save_rule(dir.path(), "allow", "Bash(touch:*)").unwrap();
    let r = run(dir.path(), "shell", json!({"command": "touch b.txt"}), ApprovalPolicy::Plan).await;
    assert!(!r.success && !dir.path().join("b.txt").exists());
}
