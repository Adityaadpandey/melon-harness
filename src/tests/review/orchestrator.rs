//! Ported from tests/test_orchestrator.py
//! Skipped: "conflict without resolver is reported, not applied" (Rust has no resolve_conflicts=false switch on the
//! orchestrator; a conflict is always re-run on top of the integrated result).
use super::super::fakes::config;
use super::SERIAL;
use crate::agent::orchestrator::{parse_graph, waves, Node, Orchestrator, WORKER_HOOK};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(super) type Edit = Box<dyn Fn(&str) -> String + Send + Sync>;

pub(super) fn git(p: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").args(args).current_dir(p).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

pub(super) fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git(p, &["init", "-q"]);
    git(p, &["config", "user.email", "t@t"]);
    git(p, &["config", "user.name", "t"]);
    git(p, &["config", "core.autocrlf", "false"]); // Windows would turn \n into \r\n and the scripted edits would match nothing
    std::fs::write(p.join("shared.py"), "def a():\n    return 0\n\n\n\n\n\ndef b():\n    return 0\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-qm", "init"]);
    dir
}

/// Stand-in for the worker agent: edits files according to a script keyed by a marker found in the prompt, after `delay`.
/// The hook is removed again when this is dropped.
pub(super) struct FakeWorker;

static WORKING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The most workers that were mid-task at the same moment since the last `install`.
pub(super) static MOST_WORKING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl FakeWorker {
    pub(super) fn install(script: Vec<(&'static str, Vec<(&'static str, Edit)>)>, delay: Duration, status: &'static str) -> FakeWorker {
        let script = Arc::new(script);
        MOST_WORKING.store(0, std::sync::atomic::Ordering::SeqCst);
        *WORKER_HOOK.lock().unwrap() = Some(Arc::new(move |cwd: PathBuf, prompt: String| {
            let script = script.clone();
            Box::pin(async move {
                use std::sync::atomic::Ordering::SeqCst;
                MOST_WORKING.fetch_max(WORKING.fetch_add(1, SeqCst) + 1, SeqCst);
                tokio::time::sleep(delay).await;
                WORKING.fetch_sub(1, SeqCst);
                for (marker, edits) in script.iter() {
                    if prompt.contains(marker) {
                        for (path, f) in edits {
                            let file = cwd.join(path);
                            let old = std::fs::read_to_string(&file).unwrap_or_default();
                            std::fs::write(&file, f(&old)).unwrap();
                        }
                    }
                }
                Some(status.to_string())
            })
        }));
        FakeWorker
    }
}

impl Drop for FakeWorker {
    fn drop(&mut self) {
        *WORKER_HOOK.lock().unwrap() = None;
    }
}

pub(super) fn node(id: &str, goal: &str, deps: &[&str]) -> Node {
    Node { id: id.into(), goal: goal.into(), deps: deps.iter().map(|s| s.to_string()).collect(), files: vec![], status: "pending".into(), branch: String::new(), changed: vec![], seconds: 0.0, note: String::new(), task_status: None }
}

fn edit(f: impl Fn(&str) -> String + Send + Sync + 'static) -> Edit {
    Box::new(f)
}

fn statuses(res: &serde_json::Value) -> Vec<(String, String)> {
    res["tasks"].as_array().unwrap().iter().map(|t| (t["id"].as_str().unwrap().to_string(), t["status"].as_str().unwrap().to_string())).collect()
}

#[test]
fn graph_validation() {
    let g = |ts: &[(&str, &[&str])]| json!({"tasks": ts.iter().map(|(i, d)| json!({"id": i, "goal": "g", "deps": d})).collect::<Vec<_>>()});
    let nodes = parse_graph(&g(&[("a", &[]), ("b", &["a"]), ("c", &[])])).unwrap();
    let ids: Vec<Vec<String>> = waves(&nodes).unwrap().iter().map(|w| w.iter().map(|i| nodes[*i].id.clone()).collect()).collect();
    assert_eq!(ids, vec![vec!["a", "c"], vec!["b"]]);
    assert!(parse_graph(&g(&[("a", &["b"]), ("b", &["a"])])).is_err());
    assert!(parse_graph(&g(&[("a", &["zzz"])])).is_err());
    assert!(parse_graph(&g(&[("a", &[]), ("a", &[])])).is_err());
}

#[tokio::test]
async fn independent_workers_run_concurrently_and_merge() {
    let _g = SERIAL.lock().await;
    let dir = repo();
    let p = dir.path();
    let script = (0..3).map(|i| (["T0", "T1", "T2"][i], vec![(["m0.py", "m1.py", "m2.py"][i], edit(|_| "x = 1\n".into()))])).collect();
    let _w = FakeWorker::install(script, Duration::from_millis(400), "verified");
    let nodes = (0..3).map(|i| node(&format!("n{i}"), &format!("T{i}"), &[])).collect();
    let res = Orchestrator::new(config(p), 4, None).run("req", nodes, true).await.unwrap();
    let peak = MOST_WORKING.load(std::sync::atomic::Ordering::SeqCst);
    assert!(peak >= 2, "workers ran one after another (at most {peak} at once)");
    assert!(res["status"] == "ok" && res["applied"] == "fast-forwarded", "{res}");
    assert!((0..3).all(|i| p.join(format!("m{i}.py")).exists()));
    // cleanup: no leftover worktrees or worker branches
    assert_eq!(git(p, &["worktree", "list"]).matches('\n').count(), 1);
    let branches = git(p, &["branch", "--list", "melon/*"]);
    assert!(branches.contains("integration") && branches.matches('\n').count() == 1);
}

#[tokio::test]
async fn same_file_disjoint_hunks_merge_cleanly_and_are_reported() {
    let _g = SERIAL.lock().await;
    let dir = repo();
    let p = dir.path();
    let script = vec![
        ("TA", vec![("shared.py", edit(|s| s.replace("def a():\n    return 0", "def a():\n    return 1")))]),
        ("TB", vec![("shared.py", edit(|s| s.replace("def b():\n    return 0", "def b():\n    return 2")))]),
    ];
    let _w = FakeWorker::install(script, Duration::ZERO, "verified");
    let res = Orchestrator::new(config(p), 4, None).run("r", vec![node("a", "TA", &[]), node("b", "TB", &[])], true).await.unwrap();
    assert_eq!(res["status"], "ok", "{res}");
    assert_eq!(res["overlapping_files"], json!({"shared.py": ["a", "b"]}));
    let text = std::fs::read_to_string(p.join("shared.py")).unwrap();
    assert!(text.contains("return 1") && text.contains("return 2"));
}

#[tokio::test]
async fn true_conflict_is_detected_and_reconciled_by_rerun() {
    let _g = SERIAL.lock().await;
    let dir = repo();
    let p = dir.path();
    let script = vec![
        ("TA", vec![("shared.py", edit(|s| s.replace("def a():\n    return 0", "def a():\n    return 'A'")))]),
        ("TB", vec![("shared.py", edit(|s| if !s.contains("'A'") { s.replace("def a():\n    return 0", "def a():\n    return 'B'") } else { format!("{s}# saw A\n") }))]),
    ];
    let _w = FakeWorker::install(script, Duration::ZERO, "verified");
    let res = Orchestrator::new(config(p), 4, None).run("r", vec![node("a", "TA", &[]), node("b", "TB", &[])], true).await.unwrap();
    assert_eq!(statuses(&res), vec![("a".into(), "done".into()), ("b".into(), "resolved".into())], "{res}");
    assert!(!std::fs::read_to_string(p.join("shared.py")).unwrap().contains("<<<<<<<"));
    let note = res["tasks"].as_array().unwrap().iter().find(|t| t["id"] == "b").unwrap()["note"].as_str().unwrap().to_string();
    assert!(note.contains("review recommended"));
}

#[tokio::test]
async fn dependent_task_sees_prerequisite_output() {
    let _g = SERIAL.lock().await;
    let dir = repo();
    let p = dir.path();
    // the second worker records whether the first worker's file is already in its checkout
    let seen = Arc::new(std::sync::Mutex::new(None));
    let s2 = seen.clone();
    *WORKER_HOOK.lock().unwrap() = Some(Arc::new(move |cwd: PathBuf, prompt: String| {
        let s2 = s2.clone();
        Box::pin(async move {
            if prompt.contains("T2") {
                // it is also told what the first one did, so it does not have to rediscover it
                *s2.lock().unwrap() = Some(cwd.join("one.txt").exists() && prompt.contains("- one (T1): changed one.txt"));
                std::fs::write(cwd.join("two.txt"), "second").unwrap();
            } else {
                std::fs::write(cwd.join("one.txt"), "first").unwrap();
            }
            Some("verified".to_string())
        })
    }));
    let res = Orchestrator::new(config(p), 4, None).run("r", vec![node("one", "T1", &[]), node("two", "T2", &["one"])], false).await;
    *WORKER_HOOK.lock().unwrap() = None;
    let res = res.unwrap();
    assert_eq!(res["status"], "ok", "{res}");
    assert_eq!(*seen.lock().unwrap(), Some(true));
    assert!(!p.join("one.txt").exists()); // apply=false leaves the user's tree alone
}

#[tokio::test]
async fn a_failed_worker_gets_one_more_try_and_is_told_why() {
    let _g = SERIAL.lock().await;
    let dir = repo();
    *WORKER_HOOK.lock().unwrap() = Some(Arc::new(move |cwd: PathBuf, prompt: String| {
        Box::pin(async move {
            // first time it only talks; told that it failed, it writes the file
            if !prompt.contains("a first attempt at this task failed") {
                return Some("failed".to_string());
            }
            std::fs::write(cwd.join("new.txt"), "x").unwrap();
            Some("verified".to_string())
        })
    }));
    let res = Orchestrator::new(config(dir.path()), 4, None).run("r", vec![node("a", "T", &[])], false).await;
    *WORKER_HOOK.lock().unwrap() = None;
    let res = res.unwrap();
    assert_eq!(statuses(&res), vec![("a".into(), "done".into())], "{res}");
    assert!(res["tasks"][0]["note"].as_str().unwrap().starts_with("second attempt") && res["tasks"][0]["goal"] == "T", "{res}");
}

#[tokio::test]
async fn apply_refuses_dirty_tree() {
    let _g = SERIAL.lock().await;
    let dir = repo();
    let p = dir.path();
    let _w = FakeWorker::install(vec![("T", vec![("new.py", edit(|_| "1".into()))])], Duration::ZERO, "verified");
    std::fs::write(p.join("shared.py"), "user edits").unwrap();
    let res = Orchestrator::new(config(p), 4, None).run("r", vec![node("a", "T", &[])], true).await.unwrap();
    assert!(res["applied"].as_str().unwrap().starts_with("skipped"));
    assert_eq!(std::fs::read_to_string(p.join("shared.py")).unwrap(), "user edits");
}

#[tokio::test]
async fn failed_dependency_skips_dependents() {
    let _g = SERIAL.lock().await;
    let dir = repo();
    // the worker dies before touching anything
    let _w = FakeWorker::install(vec![], Duration::ZERO, "failed");
    let res = Orchestrator::new(config(dir.path()), 4, None).run("r", vec![node("a", "x", &[]), node("b", "y", &["a"])], false).await.unwrap();
    assert_eq!(statuses(&res), vec![("a".into(), "failed".into()), ("b".into(), "skipped".into())]);
}

#[tokio::test]
async fn worker_that_changes_nothing_is_not_a_success() {
    let _g = SERIAL.lock().await;
    let dir = repo();
    let _w = FakeWorker::install(vec![], Duration::ZERO, "verified"); // the agent "finishes" without editing anything
    let res = Orchestrator::new(config(dir.path()), 4, None).run("r", vec![node("a", "T", &[])], true).await.unwrap();
    assert_eq!(res["status"], "partial", "{res}");
    assert!(res["tasks"][0]["note"].as_str().unwrap().starts_with("worker made no changes"));
}
