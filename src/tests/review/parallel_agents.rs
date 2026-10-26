//! Ported from tests/test_parallel_agents.py
use super::orchestrator::{git, FakeWorker};
use super::SERIAL;
use crate::agent::events::AgentEvent;
use crate::client::llm_client::CHAT_HOOK;
use crate::client::response::StreamEvent;
use crate::config::{ApprovalPolicy, Config, SubagentSpec};
use crate::tests::fakes::*;
use crate::tools::registry::create_default_registry;
use crate::config::ConfigHandle;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const READ_ONLY: &[&str] = &["read_file", "grep", "glob", "list_dir"];

fn spec(name: &str, tools: &[&str]) -> SubagentSpec {
    SubagentSpec { name: name.into(), description: format!("{name} agent"), goal_prompt: "do it".into(), allowed_tools: Some(tools.iter().map(|s| s.to_string()).collect()), max_turns: 20, timeout_seconds: 600.0, role: "default".into() }
}

fn base_cfg(dir: &std::path::Path, approval: ApprovalPolicy, subagents: Vec<SubagentSpec>) -> Config {
    let mut c = config(dir);
    c.approval = approval;
    c.auto_verify = false;
    c.check_references = false;
    c.subagents = subagents;
    c
}

/// Sub-agent children answer "child result" after 0.4s, and the hook counts how many were in flight at once.
struct SlowChild {
    peak: Arc<AtomicUsize>,
}
impl SlowChild {
    fn install() -> SlowChild {
        let (active, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let p2 = peak.clone();
        *CHAT_HOOK.lock().unwrap() = Some(Arc::new(move |_| {
            let (active, peak) = (active.clone(), p2.clone());
            Box::pin(async move {
                let now = active.fetch_add(1, SeqCst) + 1;
                peak.fetch_max(now, SeqCst);
                tokio::time::sleep(Duration::from_millis(400)).await;
                active.fetch_sub(1, SeqCst);
                text("child result")
            })
        }));
        SlowChild { peak }
    }
    fn peak(&self) -> usize {
        self.peak.load(SeqCst)
    }
}
impl Drop for SlowChild {
    fn drop(&mut self) {
        *CHAT_HOOK.lock().unwrap() = None;
    }
}

fn sub_calls(names: &[&str], goal: &str) -> Vec<StreamEvent> {
    let tools: Vec<String> = names.iter().map(|n| format!("subagent_{n}")).collect();
    calls(tools.iter().zip(names).map(|(t, n)| (t.as_str(), json!({"goal": format!("{goal} {n}")}))).collect())
}

#[tokio::test]
async fn read_only_subagents_in_one_turn_run_concurrently() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let child = SlowChild::install();
    let cfg = base_cfg(dir.path(), ApprovalPolicy::OnRequest, vec![spec("a", READ_ONLY), spec("b", READ_ONLY), spec("c", READ_ONLY)]);
    let mut a = scripted(cfg, vec![sub_calls(&["a", "b", "c"], "look"), text("merged")]).await;
    let evs = run(&mut a, "go").await;
    // starting an agent costs a few hundred ms here, so wall time cannot tell serial from parallel; three children
    // being in flight at once can
    assert_eq!(child.peak(), 3);
    let outs: Vec<String> = evs.iter().filter_map(|e| if let AgentEvent::ToolCallComplete { result, .. } = e { Some(result.output.clone()) } else { None }).collect();
    assert!(outs.len() == 3 && outs.iter().all(|o| o.contains("child result"))); // no approval prompt was needed (read-only)
}

#[tokio::test]
async fn the_concurrency_cap_is_shared_and_respected() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let child = SlowChild::install();
    let mut cfg = base_cfg(dir.path(), ApprovalPolicy::Auto, vec![spec("a", READ_ONLY), spec("b", READ_ONLY), spec("c", READ_ONLY)]);
    cfg.max_parallel_agents = 2;
    let mut a = scripted(cfg, vec![sub_calls(&["a", "b", "c"], "x"), text("done")]).await;
    let t = Instant::now();
    run(&mut a, "go").await;
    // 3 jobs, 2 slots: two waves
    assert!(child.peak() == 2 && t.elapsed() >= Duration::from_millis(750), "peak {} in {:?}", child.peak(), t.elapsed());
}

#[tokio::test]
async fn subagents_that_can_run_commands_are_serialized_and_need_approval() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let child = SlowChild::install();
    let writer: Vec<&str> = READ_ONLY.iter().copied().chain(["shell"]).collect();
    let cfg = base_cfg(dir.path(), ApprovalPolicy::Auto, vec![spec("dbg", &writer), spec("dbg2", &writer)]);
    let mut a = scripted(cfg, vec![calls(vec![("subagent_dbg", json!({"goal": "x"})), ("subagent_dbg2", json!({"goal": "y"}))]), text("done")]).await;
    run(&mut a, "go").await;
    assert_eq!(child.peak(), 1);
}

#[tokio::test]
async fn progress_events_reach_the_ui_hook() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    *CHAT_HOOK.lock().unwrap() = Some(Arc::new(|messages| {
        Box::pin(async move {
            if messages.len() <= 2 {
                calls(vec![("list_dir", json!({"path": "."}))])
            } else {
                text("found it")
            }
        })
    }));
    let seen: Arc<Mutex<Vec<(String, String, Option<String>)>>> = Arc::default();
    let cfg = base_cfg(dir.path(), ApprovalPolicy::Auto, vec![spec("inv", READ_ONLY)]);
    let mut a = scripted(cfg, vec![calls(vec![("subagent_inv", json!({"goal": "map it"}))]), text("ok")]).await;
    let s2 = seen.clone();
    *a.session.tool_registry.progress.lock().unwrap() = Some(Arc::new(move |cid, info| {
        s2.lock().unwrap().push((cid.to_string(), info["kind"].as_str().unwrap_or("").to_string(), info["name"].as_str().map(String::from)));
    }));
    run(&mut a, "go").await;
    *CHAT_HOOK.lock().unwrap() = None;
    let seen = seen.lock().unwrap();
    let kinds: Vec<&str> = seen.iter().map(|(_, k, _)| k.as_str()).collect();
    assert!(kinds[0] == "start" && kinds.contains(&"tool") && *kinds.last().unwrap() == "done", "{kinds:?}");
    assert!(seen.contains(&("c0".into(), "tool".into(), Some("list_dir".into()))));
}

#[tokio::test]
async fn parallel_tasks_tool_fans_out_into_worktrees_and_merges() {
    use super::orchestrator::Edit;
    use crate::safety::approval::{ConfirmCallback, Decision};
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git(p, &["init", "-q"]);
    git(p, &["config", "user.email", "t@t"]);
    git(p, &["config", "user.name", "t"]);
    std::fs::write(p.join("README.md"), "r").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-qm", "i"]);
    let mk = |_: ()| -> Edit { Box::new(|_| "x = 1\n".into()) };
    let _w = FakeWorker::install(vec![("YOUR task: TASK-A", vec![("a.py", mk(()))]), ("YOUR task: TASK-B", vec![("b.py", mk(()))])], Duration::from_millis(300), "verified");
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let a2 = asked.clone();
    let approve: ConfirmCallback = Arc::new(move |c| {
        a2.lock().unwrap().push(c.description.clone());
        Box::pin(async { Decision { approved: true, ..Default::default() } })
    });
    let cfg = base_cfg(p, ApprovalPolicy::OnRequest, vec![]);
    let mut agent = crate::agent::agent::Agent::new(cfg, Some(approve)).await;
    assert!(agent.session.tool_registry.get("parallel_tasks").is_some());
    *agent.session.client.script.lock().unwrap() = Some(
        vec![
            calls(vec![("parallel_tasks", json!({"tasks": [{"id": "a", "goal": "TASK-A make a.py", "files": ["a.py"]}, {"id": "b", "goal": "TASK-B make b.py", "files": ["b.py"]}]}))]),
            text("both done"),
        ]
        .into(),
    );
    let t = Instant::now();
    let evs = run(&mut agent, "do both").await;
    assert!(t.elapsed() < Duration::from_secs(5));
    let done = evs.iter().find_map(|e| if let AgentEvent::ToolCallComplete { result, .. } = e { Some(result.clone()) } else { None }).unwrap();
    assert!(done.success && done.output.contains("Parallel run: ok") && done.output.contains("fast-forwarded"), "{done:?}");
    assert!(p.join("a.py").exists() && p.join("b.py").exists());
    let asked = asked.lock().unwrap();
    assert!(!asked.is_empty() && asked[0].contains("Run 2 tasks in parallel")); // the user saw the plan before anything ran
}

#[test]
fn parallel_tasks_is_not_offered_outside_git_or_inside_workers() {
    let dir = tempfile::tempdir().unwrap();
    let registry = |c: Config| create_default_registry(ConfigHandle::new(c));
    assert!(registry(config(dir.path())).get("parallel_tasks").is_none());
    git(dir.path(), &["init", "-q"]);
    assert!(registry(config(dir.path())).get("parallel_tasks").is_some());
    let mut off = config(dir.path());
    off.enable_parallel_tasks = false;
    assert!(registry(off).get("parallel_tasks").is_none());
}
