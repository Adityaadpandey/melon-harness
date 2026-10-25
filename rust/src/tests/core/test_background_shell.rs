//! Ported from tests/test_background_shell.py. Background jobs live in one process-wide table, and every agent loop
//! in the other tests drains it too, so these tests look their own job up by command instead of counting on
//! `finished_jobs()` returning exactly one entry.
use crate::config::ConfigHandle;
use crate::tools::builtin::shell::{exit_meaning, finished_jobs, jobs_list, ShellTool};
use crate::tools::{Tool, ToolInvocation};
use serde_json::{json, Value};
use std::time::Duration;

fn inv(dir: &std::path::Path, params: Value) -> ToolInvocation {
    ToolInvocation { params, cwd: dir.to_path_buf(), epoch: 0, call_id: String::new(), progress: None }
}

fn shell(dir: &std::path::Path) -> ShellTool {
    ShellTool::new(ConfigHandle::new(super::cfg(dir, json!({}))))
}

fn job_for(command: &str) -> Option<std::sync::Arc<crate::tools::builtin::shell::BackgroundJob>> {
    jobs_list().into_iter().find(|j| j.command == command)
}

#[tokio::test]
async fn background_job_returns_fast_then_reports_done() {
    let tmp = tempfile::tempdir().unwrap();
    let cmd = "sleep 0.5; echo built-bg-one";
    let res = shell(tmp.path()).execute(&inv(tmp.path(), json!({"command": cmd, "background": true}))).await;
    assert!(res.success && res.output.contains("background job"), "{}", res.output);
    let job = job_for(cmd).expect("job registered");
    assert!(!job.done()); // still running
    assert!(finished_jobs().iter().all(|j| j.command != cmd));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(job.done() && job.tail(15).contains("built-bg-one"));
    // announced only once, whoever drains the table first
    let seen = finished_jobs().iter().chain(finished_jobs().iter()).filter(|j| j.command == cmd).count();
    assert!(seen <= 1, "announced {seen} times");
}

/// Python shortened the 15s auto-background limit to 0.3s; here the limit is a constant, so this one really waits ~16s.
#[tokio::test]
async fn slow_foreground_command_moves_to_background_on_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = shell(tmp.path());
    let quick = tool.execute(&inv(tmp.path(), json!({"command": "echo hi"}))).await;
    assert!(quick.output == "hi" && !quick.metadata.contains_key("background_job"), "{}", quick.output);
    let cmd = "echo early; sleep 17; echo late";
    let res = tool.execute(&inv(tmp.path(), json!({"command": cmd, "timeout": 60}))).await;
    assert!(res.metadata.contains_key("background_job"), "{}", res.output);
    let job = job_for(cmd).expect("job registered");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let tail = job.tail(15);
    assert!(job.done() && tail.contains("early") && tail.contains("late"), "{tail}");
}

#[tokio::test]
async fn exit_codes_are_explained_and_grep_finding_nothing_is_not_a_failure() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hello\n").unwrap();
    let tool = shell(tmp.path());
    let sh = |cmd: &'static str| {
        let (tool, dir) = (&tool, tmp.path());
        async move { tool.execute(&inv(dir, json!({"command": cmd}))).await }
    };
    let nothing = sh("grep -n nope a.txt").await;
    assert!(nothing.success && nothing.output.contains("no matches found"), "{}", nothing.output);
    assert_eq!(sh("true").await.output, "(command succeeded with no output)");
    assert!(!sh("exit 3").await.success);
    assert_eq!(exit_meaning("pytest -x", Some(1)), None); // a failing test run stays a failure
    assert_eq!(exit_meaning("make", Some(137)).as_deref(), Some("killed by signal 9, often out of memory"));
}
