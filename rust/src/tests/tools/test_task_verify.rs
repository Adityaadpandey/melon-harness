//! Ported from tests/test_task_verify.py: the request lifecycle (run, verify, repair), the signature guard and
//! backup/undo, all driven by scripted model replies and throwaway git repos.
use super::git;
use crate::agent::agent::Agent;
use crate::agent::events::AgentEvent;
use crate::agent::task::{TaskState, TaskStatus};
use crate::agent::verify::{backup, select_checks, signature_breaks, undo};
use crate::config::{ApprovalPolicy, Config, VerifyRule};
use crate::tests::fakes::{call, calls, config, scripted, text};
use crate::util::trace::Tracer;
use serde_json::{json, Value};
use std::fs;
use std::sync::Arc;

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q"]);
    git(p, &["config", "user.email", "t@t"]);
    git(p, &["config", "user.name", "t"]);
    fs::write(p.join("keep.txt"), "x").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-qm", "init"]);
    d
}

fn cfg_for(repo: &std::path::Path, tweak: impl FnOnce(&mut Config)) -> Config {
    let mut c = config(repo);
    c.approval = ApprovalPolicy::Auto;
    tweak(&mut c);
    c
}

async fn go(agent: &mut Agent, msg: &str) -> Vec<AgentEvent> {
    crate::tests::fakes::run(agent, msg).await
}

fn report(events: &[AgentEvent]) -> Value {
    events.iter().find_map(|e| if let AgentEvent::AgentEnd { report, .. } = e { Some(report.clone()) } else { None }).expect("no agent_end event")
}

fn write(name: &str) -> Vec<crate::client::response::StreamEvent> {
    call("write_file", json!({"path": name, "content": "1"}))
}

/// The last message of request number `n` sent to the scripted model.
fn last_content(agent: &Agent, n: usize) -> String {
    let reqs = agent.session.client.requests.lock().unwrap();
    reqs[n].last().unwrap()["content"].to_string()
}

fn request_count(agent: &Agent) -> usize {
    agent.session.client.requests.lock().unwrap().len()
}

#[test]
fn invalid_transitions_are_rejected() {
    // Python raises InvalidTransition; the Rust port records the bad move in the trace and keeps the old status
    let mut t = TaskState::new("x", Arc::new(Tracer::new("t", None)), 10.0, 1);
    t.transition(TaskStatus::Failed, "");
    t.transition(TaskStatus::Verified, "");
    assert_eq!(t.status, TaskStatus::Failed);
}

#[tokio::test]
async fn failed_check_triggers_repair_then_verifies() {
    let d = repo();
    let cfg = cfg_for(d.path(), |c| c.verify_commands = vec!["test -f ok.txt".into()]);
    let mut a = scripted(cfg, vec![write("bad.txt"), text("done"), write("ok.txt"), text("fixed")]).await;
    let rep = report(&go(&mut a, "go").await);
    assert_eq!(rep["status"], "verified", "{rep}");
    assert_eq!(rep["repairs"], 1);
    let mut changed: Vec<String> = rep["changed_files"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    changed.sort();
    assert_eq!(changed, vec!["bad.txt", "ok.txt"]);
    // the failure output reached the model as the repair request
    assert!(last_content(&a, 2).contains("FAILED"));
}

#[tokio::test]
async fn persistent_failure_is_reported_not_claimed_as_success() {
    let d = repo();
    let cfg = cfg_for(d.path(), |c| {
        c.verify_commands = vec!["false".into()];
        c.max_repairs = 1;
    });
    let mut a = scripted(cfg, vec![write("a.txt"), text("done"), text("tried")]).await;
    let rep = report(&go(&mut a, "go").await);
    assert_eq!(rep["status"], "verification_failed", "{rep}");
    assert_eq!(rep["repairs"], 1);
}

#[tokio::test]
async fn read_only_run_needs_no_verification() {
    let d = repo();
    let cfg = cfg_for(d.path(), |c| c.verify_commands = vec!["false".into()]);
    let mut a = scripted(cfg, vec![text("the answer")]).await;
    assert_eq!(report(&go(&mut a, "go").await)["status"], "done_no_changes");
}

#[tokio::test]
async fn preexisting_user_edits_are_not_blamed_on_the_agent() {
    let d = repo();
    fs::write(d.path().join("keep.txt"), "user edit in progress").unwrap();
    let cfg = cfg_for(d.path(), |c| c.verify_commands = vec!["false".into()]);
    let mut a = scripted(cfg, vec![text("nothing to do")]).await;
    let rep = report(&go(&mut a, "go").await);
    assert_eq!(rep["status"], "done_no_changes");
    assert_eq!(rep["changed_files"], json!([]));
}

#[tokio::test]
async fn changes_without_any_checks_are_labelled_unverified() {
    let d = repo();
    let cfg = cfg_for(d.path(), |c| c.auto_verify = false);
    let mut a = scripted(cfg, vec![write("n.txt"), text("done")]).await;
    assert_eq!(report(&go(&mut a, "go").await)["status"], "unverified");
}

#[tokio::test]
async fn repeated_identical_calls_end_the_task() {
    let d = repo();
    let cfg = cfg_for(d.path(), |c| c.auto_verify = false);
    let same = (0..12).map(|_| calls(vec![("list_dir", json!({"path": "."}))])).collect();
    let mut a = scripted(cfg, same).await;
    let events = go(&mut a, "go").await;
    assert_eq!(report(&events)["status"], "failed");
    assert!(events.iter().any(|e| matches!(e, AgentEvent::AgentError { error } if error.contains("Stopped"))), "{events:?}");
}

#[tokio::test]
async fn code_pasted_instead_of_written_gets_one_nudge() {
    let d = repo();
    let cfg = cfg_for(d.path(), |c| c.auto_verify = false);
    let mut a = scripted(cfg, vec![text("Here you go:\n```py\nx=1\n```"), write("x.py"), text("applied")]).await;
    let rep = report(&go(&mut a, "create x.py with x = 1").await);
    assert_eq!(rep["status"], "unverified", "{rep}");
    assert_eq!(rep["changed_files"], json!(["x.py"]));
}

#[tokio::test]
async fn nudge_happens_only_once() {
    let d = repo();
    let cfg = cfg_for(d.path(), |c| c.auto_verify = false);
    let mut a = scripted(cfg, vec![text("```py\na\n```"), text("```py\nb\n```")]).await;
    // nudged once, then accepted
    assert_eq!(report(&go(&mut a, "create x.py").await)["status"], "done_no_changes");
}

#[test]
fn related_tests_expand_only_for_specs_that_import_the_changed_file() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    fs::create_dir_all(p.join("app/unit")).unwrap();
    fs::write(p.join("app/unit/a.spec.ts"), "import { x } from \"../src/lib/thing\";\n").unwrap();
    fs::write(p.join("app/unit/b.spec.ts"), "import { y } from \"../src/lib/other\";\n").unwrap();
    let rule = VerifyRule { paths: vec!["app/".into()], cwd: "app".into(), tests_glob: Some("unit/**/*.spec.ts".into()), commands: vec!["run {tests}".into()] };
    let mut cfg = config(p);
    cfg.verify_rules = vec![rule];
    cfg.auto_verify = false;
    assert_eq!(select_checks(&cfg, &["app/src/lib/thing.ts".to_string()]), vec![("run unit/a.spec.ts".to_string(), "app".to_string())]);
    assert!(select_checks(&cfg, &["app/src/lib/none.ts".to_string()]).is_empty());
}

#[tokio::test]
async fn backup_and_undo_restore_edits_and_remove_created_files() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q"]);
    git(p, &["config", "user.email", "t@t"]);
    git(p, &["config", "user.name", "t"]);
    fs::write(p.join("a.txt"), "one\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-qm", "init"]);
    fs::write(p.join("a.txt"), "two (user's unsaved work)\n").unwrap(); // dirty tree before the run
    let r = backup(p).await.unwrap();
    fs::write(p.join("a.txt"), "agent edit\n").unwrap();
    fs::write(p.join("new.txt"), "made by the run\n").unwrap();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let (restored, deleted, skipped) = undo(p, &r, &s(&["a.txt", "new.txt"]), &s(&["new.txt"])).await;
    assert_eq!(fs::read_to_string(p.join("a.txt")).unwrap(), "two (user's unsaved work)\n");
    assert!(!p.join("new.txt").exists());
    assert_eq!(restored, vec!["a.txt"]);
    assert_eq!(deleted, vec!["new.txt"]);
    assert!(skipped.is_empty());
    assert!(backup(p).await.is_some());
    let kept = std::process::Command::new("git").args(["rev-parse", "refs/melon/backup"]).current_dir(p).output().unwrap();
    assert!(kept.status.success());
}

#[tokio::test]
async fn a_note_typed_mid_check_stops_the_check_and_is_worked_on_now() {
    // the first check would hang for 30s; the user's note comes in while it runs
    let d = repo();
    let cfg = cfg_for(d.path(), |c| c.verify_commands = vec!["test -f steered.txt || sleep 30".into()]);
    let mut a = scripted(cfg, vec![write("a.txt"), text("done"), write("steered.txt"), text("fixed the buttons too")]).await;
    let started = std::time::Instant::now();
    let steering = a.steering.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let watcher = async {
        let (mut events, mut steered) = (vec![], false);
        while let Some(e) = rx.recv().await {
            if let AgentEvent::Notice(m) = &e {
                if m.contains("Verifying") && !steered {
                    steering.lock().unwrap().push("the white buttons are broken too".into());
                    steered = true;
                }
            }
            events.push(e);
        }
        events
    };
    let (_, events) = tokio::join!(a.run("go", tx), watcher);
    let rep = report(&events);
    assert!(started.elapsed().as_secs() < 15);
    assert_eq!(rep["status"], "verified", "{rep}");
    assert_eq!(rep["repairs"], 0);
    assert!(last_content(&a, 2).contains("white buttons"));
}

#[tokio::test]
async fn a_dropped_parameter_is_sent_back_once_even_though_the_checks_pass() {
    // the benchmark miss: the fix removed `seen` and the agent's own test still passed
    let d = repo();
    let p = d.path();
    fs::write(p.join("lib.py"), "def unique_by(items, key, seen=set()):\n    return items\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-qm", "lib"]);
    let cfg = cfg_for(p, |c| c.verify_commands = vec!["true".into()]);
    let mut a = scripted(
        cfg,
        vec![
            call("read_file", json!({"path": "lib.py"})),
            call("write_file", json!({"path": "lib.py", "content": "def unique_by(items, key):\n    return items\n"})),
            text("done"),
            call("write_file", json!({"path": "lib.py", "content": "def unique_by(items, key, seen=None):\n    return items\n"})),
            text("kept it"),
        ],
    )
    .await;
    let rep = report(&go(&mut a, "fix the shared state bug in unique_by").await);
    let sent = last_content(&a, 3);
    assert!(sent.contains("unique_by() no longer accepts `seen`"), "{sent}");
    assert_eq!(rep["status"], "verified", "{rep}");
    assert_eq!(request_count(&a), 5); // one nudge, not a loop
}

#[tokio::test]
async fn signature_guard_skips_what_the_user_asked_for_and_functions_that_moved() {
    let d = repo();
    let p = d.path();
    fs::write(p.join("a.py"), "def load(path, cache=None):\n    pass\n\ndef helper(x):\n    pass\n").unwrap();
    fs::write(p.join("b.py"), "").unwrap();
    fs::write(p.join("test_a.py"), "def test_x(tmp):\n    pass\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-qm", "base"]);
    fs::write(p.join("a.py"), "def load(path):\n    pass\n").unwrap();
    fs::write(p.join("b.py"), "def helper(x):\n    pass\n").unwrap(); // moved, not removed
    fs::write(p.join("test_a.py"), "def test_x():\n    pass\n").unwrap(); // tests are nobody's API
    let changed: Vec<String> = ["a.py", "b.py", "test_a.py"].iter().map(|s| s.to_string()).collect();
    assert_eq!(signature_breaks(p, &changed, "tidy up a.py").await, vec!["a.py: load() no longer accepts `cache`"]);
    assert!(signature_breaks(p, &changed, "drop the cache parameter from load").await.is_empty());
}

#[tokio::test]
async fn a_check_that_was_already_failing_is_not_repaired_and_the_run_is_not_called_verified() {
    let d = repo();
    let cfg = cfg_for(d.path(), |c| c.verify_commands = vec!["echo 'types.ts(3,1): error TS2307: stale'; exit 1".into()]);
    let mut a = scripted(cfg, vec![write("a.txt"), text("done")]).await;
    let rep = report(&go(&mut a, "go").await);
    assert_eq!(rep["repairs"], 0, "{rep}");
    assert_eq!(rep["status"], "unverified", "{rep}");
    assert_eq!(rep["checks"][0]["preexisting"], true);
    assert_eq!(request_count(&a), 2); // no repair round trip
}
