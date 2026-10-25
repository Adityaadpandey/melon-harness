//! Ported from tests/test_agent_loop.py: the agent loop end to end against scripted model replies.
//!
//! Differences from the Python file, all because the Rust pieces are concrete types rather than monkey-patchable ones:
//! - "compacts once and retries" and "cold cache" script the summary as a model reply instead of faking `_compact`.
//! - "interrupted during the review" parks the run in a scout sub-agent that never answers, instead of in the review
//!   pipeline (the pipeline spawns its own un-scriptable agents); it checks the same Ctrl+C bookkeeping.
//! - "cleared result is not treated as still in context" marks the result as cleared directly instead of wrapping
//!   `add_tool_result`.
use super::{call_as, cfg, git_init, summary_turn, tool_texts};
use crate::agent::agent::{next_within, Agent};
use crate::agent::events::AgentEvent;
use crate::agent::persistence::PersistenceManager;
use crate::client::response::{StreamEvent, TokenUsage, ToolCall};
use crate::config::{ConfigHandle, HookConfig, HookTrigger};
use crate::context::MessageItem;
use crate::hooks::HookSystem;
use crate::tests::fakes::{answer, call, calls, error, run, scripted, text};
use crate::tools::builtin::graph::GraphTool;
use crate::tools::builtin::todo::TodosTool;
use crate::tools::registry::create_default_registry;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::ui::app::Cli;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------------------------------------ small helpers

fn inv(dir: &Path, params: Value) -> ToolInvocation {
    ToolInvocation { params, cwd: dir.to_path_buf(), epoch: 0, call_id: String::new(), progress: None }
}

fn starts(evs: &[AgentEvent]) -> usize {
    evs.iter().filter(|e| matches!(e, AgentEvent::ToolCallStart { .. })).count()
}

fn notices(evs: &[AgentEvent]) -> Vec<String> {
    evs.iter().filter_map(|e| if let AgentEvent::Notice(m) = e { Some(m.clone()) } else { None }).collect()
}

fn completed(evs: &[AgentEvent]) -> Vec<(String, ToolResult)> {
    evs.iter().filter_map(|e| if let AgentEvent::ToolCallComplete { call_id, result, .. } = e { Some((call_id.clone(), result.clone())) } else { None }).collect()
}

fn text_completes(evs: &[AgentEvent]) -> Vec<String> {
    evs.iter().filter_map(|e| if let AgentEvent::TextComplete(t) = e { Some(t.clone()) } else { None }).collect()
}

fn has_error(evs: &[AgentEvent]) -> bool {
    evs.iter().any(|e| matches!(e, AgentEvent::AgentError { .. }))
}

fn user_messages(a: &Agent) -> Vec<String> {
    a.session.context_manager.lock().unwrap().messages.iter().filter(|m| m.role == "user").map(|m| m.content.clone()).collect()
}

fn requests(a: &Agent) -> Vec<Vec<Value>> {
    a.session.client.requests.lock().unwrap().clone()
}

fn last_content(req: &[Value]) -> String {
    req.last().unwrap()["content"].as_str().unwrap_or("").to_string()
}

/// A model reply that ends with the given usage (to play with the token budget).
fn with_usage(mut ev: Vec<StreamEvent>, u: TokenUsage) -> Vec<StreamEvent> {
    *ev.last_mut().unwrap() = StreamEvent::MessageComplete { finish_reason: None, usage: Some(u), reasoning: None, text: None };
    ev
}

fn over_limit() -> TokenUsage {
    TokenUsage { prompt_tokens: 60_000, completion_tokens: 100, total_tokens: 60_100, ..Default::default() }
}

/// Plenty of older conversation, so a compaction cut exists.
fn long_history(a: &Agent, pairs: usize) {
    let mut cm = a.session.context_manager.lock().unwrap();
    for i in 0..pairs {
        cm.add_user_message(&format!("old message {i} {}", "word ".repeat(200)));
        cm.add_assistant_message(Some(&format!("old reply {i} {}", "word ".repeat(200))), None, None, None);
    }
}

// ------------------------------------------------------------------------------------------------ the loop

#[tokio::test]
async fn tool_call_arguments_are_valid_json_in_history() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![call("read_file", json!({"path": "a.txt"})), text("done")]).await;
    run(&mut agent, "go").await;
    let sent = &requests(&agent)[1];
    let tc = &sent.iter().find(|m| m.get("tool_calls").is_some()).unwrap()["tool_calls"][0];
    let args: Value = serde_json::from_str(tc["function"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(args, json!({"path": "a.txt"}));
}

#[tokio::test]
async fn api_error_stops_loop_and_leaves_no_empty_assistant_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![error("boom")]).await;
    let events = run(&mut agent, "go").await;
    assert!(has_error(&events));
    assert_eq!(requests(&agent).len(), 1); // no second request was made
    let roles: Vec<String> = agent.session.context_manager.lock().unwrap().get_messages().iter().map(|m| m["role"].as_str().unwrap().to_string()).collect();
    assert_eq!(roles, vec!["system", "user"]);
}

#[tokio::test]
async fn malformed_arguments_are_rejected_not_executed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![call("write_file", json!({"raw_arguments": "{\"path\": \"x"})), text("ok")]).await;
    let events = run(&mut agent, "go").await;
    let (_, done) = &completed(&events)[0];
    assert!(!done.success);
    assert!(done.error.as_deref().unwrap_or("").contains("valid JSON"), "{:?}", done.error);
    assert!(!tmp.path().join("x").exists());
}

/// Takes 200ms and answers with its tag: five of them in one turn only finish in time if they overlap.
struct SleepTool;

#[async_trait]
impl Tool for SleepTool {
    fn name(&self) -> &str {
        "sleepy"
    }
    fn description(&self) -> String {
        "sleeps".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn schema(&self) -> Value {
        schema_of(json!({"tag": {"type": "string", "default": "x"}}), &[])
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        tokio::time::sleep(Duration::from_millis(200)).await;
        ToolResult::ok(inv.str("tag").unwrap_or("x"))
    }
}

#[tokio::test]
async fn read_only_calls_run_concurrently_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let pairs: Vec<(&str, Value)> = (0..5).map(|i| ("sleepy", json!({"tag": i.to_string()}))).collect();
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![calls(pairs), text("done")]).await;
    agent.session.tool_registry.register(Arc::new(SleepTool));
    let t = Instant::now();
    let events = run(&mut agent, "go").await;
    assert!(t.elapsed() < Duration::from_millis(600), "serial would be >= 1.0s, took {:?}", t.elapsed()); // serial would be >= 1.0s
    let outs: Vec<String> = completed(&events).into_iter().map(|(_, r)| r.output).collect();
    assert_eq!(outs, vec!["0", "1", "2", "3", "4"]);
}

#[tokio::test]
async fn writes_are_barriers_and_stay_ordered() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![calls(vec![("write_file", json!({"path": "f.txt", "content": "v1"})), ("read_file", json!({"path": "f.txt"}))]), text("done")]).await;
    let events = run(&mut agent, "go").await;
    assert!(completed(&events)[1].1.output.contains("v1"));
}

#[tokio::test]
async fn no_callback_means_no_silent_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({"approval": "on-request"})), vec![call("shell", json!({"command": "touch n.txt"})), text("ok")]).await;
    run(&mut agent, "go").await;
    assert!(!tmp.path().join("n.txt").exists());
}

#[test]
fn empty_tool_output_still_has_content_key() {
    let mut m = MessageItem::new("tool", "");
    m.tool_call_id = Some("x".into());
    assert_eq!(m.to_value()["content"], "(no output)");
    let mut a = MessageItem::new("assistant", "");
    a.tool_calls = vec![json!({"id": "1"})];
    assert_eq!(a.to_value()["content"], "");
}

#[tokio::test]
async fn a_stalled_model_stream_ends_when_the_time_budget_does() {
    let stalled = futures::stream::unfold(0, |n| async move {
        if n == 0 {
            Some((StreamEvent::TextDelta("first".into()), 1))
        } else {
            tokio::time::sleep(Duration::from_secs(30)).await;
            None
        }
    });
    let mut s: futures::stream::BoxStream<'static, StreamEvent> = Box::pin(stalled);
    let deadline = Instant::now() + Duration::from_millis(200);
    let mut got = vec![];
    while let Some(ev) = next_within(deadline, &mut s).await {
        let stop = matches!(ev, StreamEvent::Error(_));
        got.push(ev);
        if stop {
            break;
        }
    }
    assert!(matches!(&got[0], StreamEvent::TextDelta(t) if t == "first"));
    assert!(matches!(&got[1], StreamEvent::Error(e) if e.contains("Time budget")), "{got:?}");
}

#[tokio::test]
async fn session_autosave_resume_and_continue_by_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let config = cfg(&work, json!({"auto_verify": false}));
    let mut cli = Cli::new(config.clone());
    cli.new_agent(None).await;
    let sid = {
        let a = cli.agent.as_mut().unwrap();
        {
            let mut cm = a.session.context_manager.lock().unwrap();
            cm.add_user_message("Fix the blank line bug in the description\n\n---\nPlaybooks selected: lots of text");
            cm.add_assistant_message(None, Some(vec![json!({"id": "c1", "type": "function", "function": {"name": "grep", "arguments": "{}"}})]), None, None);
            cm.add_tool_result("c1", "found it");
        }
        a.session.increment_turn();
        a.session.session_id()
    };
    cli.autosave();
    let pm = PersistenceManager::new();
    let row = pm.list_sessions().into_iter().find(|r| r["session_id"] == sid).expect("the session was saved");
    assert_eq!(row["title"], "Fix the blank line bug in the description");
    assert_eq!(row["cwd"], work.to_string_lossy().as_ref());
    assert_eq!(pm.latest_for(&work.to_string_lossy()), Some(sid.clone()));
    assert_eq!(pm.latest_for("/elsewhere"), None);

    let mut cli2 = Cli::new(config);
    let ask: crate::safety::approval::ConfirmCallback = Arc::new(|_| Box::pin(async { crate::safety::approval::Decision { approved: true, ..Default::default() } }));
    cli2.new_agent(Some(ask)).await;
    assert!(cli2.resume(&sid).await && !cli2.resume("nope").await);
    let b = cli2.agent.as_ref().unwrap();
    assert!(b.session.approval_manager.has_callback()); // a resumed session can still ask
    let roles: Vec<String> = b.session.context_manager.lock().unwrap().get_messages().iter().map(|m| m["role"].as_str().unwrap().to_string()).collect();
    assert_eq!(roles[1..], ["user", "assistant", "tool"]);
    assert_eq!(b.session.session_id(), sid);
    assert_eq!(b.session.turns(), 1);
}

#[test]
fn old_session_files_without_cwd_still_load() {
    let id = format!("old-{}", uuid::Uuid::new_v4());
    let old = json!({"session_id": id, "created_at": "2026-01-01T00:00:00", "updated_at": "2026-01-01T00:00:00", "turn_count": 2,
                     "messages": [], "total_usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0, "cached_tokens": 0}});
    crate::tests::fakes::init();
    let pm = PersistenceManager::new();
    std::fs::write(crate::config::loader::get_data_dir().join("sessions").join(format!("{id}.json")), old.to_string()).unwrap();
    assert_eq!(pm.load_session(&id).unwrap().cwd, "");
    assert_eq!(pm.list_sessions().into_iter().find(|r| r["session_id"] == id.as_str()).unwrap()["title"], "");
}

#[tokio::test]
async fn an_identical_lookup_is_answered_once_until_something_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.py"), "needle = 1\n".repeat(50)).unwrap();
    let g = json!({"pattern": "needle", "path": "."});
    let script = vec![call_as("grep", g.clone(), "a"), call_as("grep", g.clone(), "b"), call_as("write_file", json!({"path": "b.py", "content": "needle = 2\n"}), "w"), call_as("grep", g, "c"), text("done")];
    let mut agent = scripted(cfg(tmp.path(), json!({})), script).await;
    let events = run(&mut agent, "go").await;
    let outs: std::collections::HashMap<String, String> = completed(&events).into_iter().map(|(id, r)| (id, r.output)).collect();
    assert!(outs["a"].contains("needle") && outs["b"].contains("unchanged"), "{outs:?}"); // repeat: a pointer, not the same text again
    assert!(outs["c"].contains("needle") && !outs["c"].contains("unchanged")); // the write invalidated it
}

#[tokio::test]
async fn a_cleared_result_is_not_treated_as_still_in_context() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.py"), "needle = 1\n").unwrap();
    let agent = scripted(cfg(tmp.path(), json!({})), vec![]).await;
    let (reg, hs, g) = (agent.session.tool_registry.clone(), &agent.session.hook_system, json!({"pattern": "needle", "path": "."}));
    // the model's earlier answer is in the history ...
    let first = reg.invoke("grep", g.clone(), tmp.path(), hs, None, "a").await;
    agent.session.context_manager.lock().unwrap().add_tool_result("a", &first.output);
    // ... and pruning clears it at once
    agent.session.context_manager.lock().unwrap().messages.last_mut().unwrap().pruned = true;
    let second = reg.invoke("grep", g, tmp.path(), hs, None, "b").await;
    assert!(!second.output.contains("unchanged"), "{}", second.output);
}

#[tokio::test]
async fn two_workspaces_never_share_a_cached_answer() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, body) in [("one", "needle one"), ("two", "needle two")] {
        std::fs::create_dir_all(tmp.path().join(name)).unwrap();
        std::fs::write(tmp.path().join(name).join("f.txt"), body).unwrap();
    }
    let c = cfg(&tmp.path().join("one"), json!({}));
    let reg = create_default_registry(ConfigHandle::new(c.clone()));
    let hooks = HookSystem::new(&c);
    let g = json!({"pattern": "needle", "path": "."});
    let a = reg.invoke("grep", g.clone(), &tmp.path().join("one"), &hooks, None, "x1").await;
    let b = reg.invoke("grep", g, &tmp.path().join("two"), &hooks, None, "x2").await;
    assert!(a.output.contains("one") && b.output.contains("two") && !b.output.contains("unchanged"), "{} / {}", a.output, b.output);
}

#[tokio::test]
async fn budget_counts_new_work_not_a_resent_cached_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let cfg = cfg(tmp.path(), json!({"auto_verify": false, "max_total_tokens": 10_000}));
    // 100k prompt, all but 500 of it cached: huge in total, tiny as new work
    let turn = |i: usize| with_usage(call_as("read_file", json!({"path": "a.txt"}), &format!("c{i}")), TokenUsage { prompt_tokens: 100_000, completion_tokens: 50, total_tokens: 100_050, cached_tokens: 99_500, ..Default::default() });
    let mut script: Vec<_> = (0..8).map(turn).collect();
    script.push(text("done"));
    let mut agent = scripted(cfg, script).await;
    let events = run(&mut agent, "go").await;
    assert!(!notices(&events).iter().any(|m| m.to_lowercase().contains("budget")), "{:?}", notices(&events));
}

#[tokio::test]
async fn a_run_stopped_by_the_budget_still_reports_the_files_it_changed() {
    let tmp = tempfile::tempdir().unwrap();
    git_init(tmp.path());
    let cfg = cfg(tmp.path(), json!({"auto_verify": false, "max_total_tokens": 1000}));
    let ev = with_usage(call_as("write_file", json!({"path": "new.txt", "content": "x"}), "w"), TokenUsage { prompt_tokens: 5000, completion_tokens: 10, total_tokens: 5010, ..Default::default() });
    let mut agent = scripted(cfg, vec![ev, text("partial answer")]).await;
    let events = run(&mut agent, "go").await;
    let Some(AgentEvent::AgentEnd { report, .. }) = events.iter().find(|e| matches!(e, AgentEvent::AgentEnd { .. })) else { panic!("no AgentEnd") };
    assert_eq!(report["changed_files"], json!(["new.txt"]));
}

#[tokio::test]
async fn a_budget_stop_runs_the_checks_and_reports_a_broken_tree() {
    let tmp = tempfile::tempdir().unwrap();
    git_init(tmp.path());
    let cfg = cfg(tmp.path(), json!({"max_total_tokens": 1000, "verify_commands": ["exit 3"]}));
    let ev = with_usage(call_as("write_file", json!({"path": "new.txt", "content": "x"}), "w"), TokenUsage { prompt_tokens: 5000, completion_tokens: 10, total_tokens: 5010, ..Default::default() });
    let mut agent = scripted(cfg, vec![ev, text("partial answer")]).await;
    let events = run(&mut agent, "go").await;
    let Some(AgentEvent::AgentEnd { report, .. }) = events.iter().find(|e| matches!(e, AgentEvent::AgentEnd { .. })) else { panic!("no AgentEnd") };
    assert_eq!(report["status"], "verification_failed");
    assert_eq!(report["checks"][0]["exit_code"], 3);
}

#[tokio::test]
async fn a_run_that_cannot_ask_for_approval_stops_at_the_first_refusal() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg(tmp.path(), json!({"approval": "on-request", "auto_verify": false}));
    let mut turns: Vec<_> = (0..5).map(|i| call_as("write_file", json!({"path": "n.txt", "content": "x"}), &format!("w{i}"))).collect();
    turns.push(text("done"));
    let mut agent = scripted(cfg, turns).await; // no confirmation callback: single-run mode
    let events = run(&mut agent, "go").await;
    let errs: Vec<&String> = events.iter().filter_map(|e| if let AgentEvent::AgentError { error } = e { Some(error) } else { None }).collect();
    assert!(!errs.is_empty() && errs[0].contains("--approval auto-edit"), "{errs:?}");
    assert_eq!(requests(&agent).len(), 1); // one model call, not a retry loop
    assert!(!tmp.path().join("n.txt").exists());
}

#[tokio::test]
async fn steering_note_lands_after_the_tool_round_and_before_the_next_model_call() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![call("read_file", json!({"path": "a.txt"})), text("done")]).await;
    agent.steering.lock().unwrap().push("use python 3.12".into());
    run(&mut agent, "go").await;
    assert!(last_content(&requests(&agent)[1]).contains("use python 3.12"));
    assert!(agent.steering.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provider_context_overflow_compacts_once_and_retries() {
    let tmp = tempfile::tempdir().unwrap();
    // the refusal first, then the summary the compaction asks for, then the real answer
    let script = vec![error("This model's maximum context length is 128000 tokens"), summary_turn("old work", TokenUsage { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2, ..Default::default() }), text("done")];
    let mut agent = scripted(cfg(tmp.path(), json!({})), script).await;
    long_history(&agent, 30); // enough history for a compaction cut to exist
    let events = run(&mut agent, "go").await;
    assert!(!has_error(&events), "{events:?}");
    assert!(notices(&events).iter().any(|m| m.contains("too long")));
    assert_eq!(answer(&events).as_deref(), Some("done"));
    assert_eq!(requests(&agent).len(), 3); // refused, summarised, answered
}

#[tokio::test]
async fn a_cold_cache_after_a_pause_shrinks_history_before_the_first_call() {
    let tmp = tempfile::tempdir().unwrap();
    // pi mode skips this tidying; one second of "cache time" keeps the test from needing a long pause
    let cfg = cfg(tmp.path(), json!({"pi_mode": false, "idle_compact_at_tokens": 1000, "cache_ttl_seconds": 1}));
    let mut agent = scripted(cfg, vec![summary_turn("old work", TokenUsage { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2, ..Default::default() }), text("done")]).await;
    long_history(&agent, 30);
    agent.session.context_manager.lock().unwrap().set_latest_usage(over_limit());
    agent.last_model_call = Some(Instant::now() - Duration::from_secs(5)); // long pause
    let events = run(&mut agent, "go").await;
    assert!(notices(&events).iter().any(|m| m.contains("Cache had expired")), "{:?}", notices(&events));

    // warm cache: leave history alone
    *agent.session.client.script.lock().unwrap() = Some(vec![text("done")].into());
    agent.session.client.requests.lock().unwrap().clear();
    agent.session.context_manager.lock().unwrap().set_latest_usage(over_limit());
    agent.last_model_call = Some(Instant::now());
    let events = run(&mut agent, "again").await;
    assert!(!notices(&events).iter().any(|m| m.contains("Cache had expired")));
    assert_eq!(requests(&agent).len(), 1); // just the answer, no summary request
}

#[tokio::test]
async fn read_only_request_is_cut_off_at_the_call_budget_and_forced_to_answer() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let mut script: Vec<_> = (0..3).map(|i| call_as("read_file", json!({"path": "a.txt"}), &format!("c{i}"))).collect();
    script.push(text("answer")); // the 4th reply is the forced wrap-up
    let mut agent = scripted(cfg(tmp.path(), json!({"read_call_budget": 3})), script).await;
    let events = run(&mut agent, "explain what a.txt holds").await;
    assert_eq!(starts(&events), 3);
    assert!(notices(&events).iter().any(|m| m.contains("Call budget reached")));
    assert!(!text_completes(&events).is_empty());
}

#[tokio::test]
async fn requests_that_change_files_are_not_capped_by_the_read_budget() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let mut script: Vec<_> = (0..3).map(|i| call_as("read_file", json!({"path": "a.txt"}), &format!("c{i}"))).collect();
    script.push(text("done"));
    let mut agent = scripted(cfg(tmp.path(), json!({"read_call_budget": 1})), script).await;
    let events = run(&mut agent, "fix the typo in a.txt").await;
    assert_eq!(starts(&events), 3);
}

#[tokio::test]
async fn a_run_that_starts_read_only_but_edits_is_no_longer_capped() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let script = vec![
        call_as("edit", json!({"path": "a.txt", "old_string": "hi", "new_string": "yo"}), "e1"),
        call_as("read_file", json!({"path": "a.txt"}), "r1"),
        call_as("read_file", json!({"path": "a.txt"}), "r2"),
        call_as("read_file", json!({"path": "a.txt"}), "r3"),
        text("done"),
    ];
    let mut agent = scripted(cfg(tmp.path(), json!({"read_call_budget": 2})), script).await;
    let events = run(&mut agent, "explain a.txt and why it is short").await; // reads as read-only, then edits
    assert_eq!(starts(&events), 4);
}

#[tokio::test]
async fn more_allows_exactly_that_many_calls_for_one_request_only() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({"read_call_budget": 20})), vec![text("ok")]).await;
    agent.extra_calls = 10;
    run(&mut agent, "explain things").await;
    assert_eq!(agent.call_limit(), 10);
    assert_eq!(agent.extra_calls, 0);
    *agent.session.client.script.lock().unwrap() = Some(vec![text("ok")].into());
    run(&mut agent, "explain more").await;
    assert_eq!(agent.call_limit(), 20);
}

#[tokio::test]
async fn a_capped_answer_with_unverifiable_refs_is_not_sent_back_for_a_second_answer() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let cfg = cfg(tmp.path(), json!({"read_call_budget": 1, "check_references": true}));
    // the wrap-up cites a file that does not exist; with no tools left, a correction round would only cost another full-context call
    let mut agent = scripted(cfg, vec![call_as("read_file", json!({"path": "a.txt"}), "c0"), text("see ghost/missing.py:3"), text("second answer")]).await;
    let events = run(&mut agent, "explain what a.txt holds").await;
    assert_eq!(text_completes(&events).len(), 1);
}

#[tokio::test]
async fn parallel_calls_in_one_turn_count_as_one_round_of_the_read_budget() {
    let tmp = tempfile::tempdir().unwrap();
    for n in "abcde".chars() {
        std::fs::write(tmp.path().join(format!("{n}.txt")), n.to_string()).unwrap();
    }
    // five reads at once, then two more single reads: 3 rounds, so the third is cut off
    let five: Vec<(&str, Value)> = "abcde".chars().map(|n| ("read_file", json!({"path": format!("{n}.txt")}))).collect();
    let script = vec![calls(five), call_as("read_file", json!({"path": "a.txt"}), "r1"), call_as("read_file", json!({"path": "b.txt"}), "r2"), text("answer")];
    let mut agent = scripted(cfg(tmp.path(), json!({"read_call_budget": 2})), script).await;
    let events = run(&mut agent, "explain what these files hold").await;
    assert_eq!(starts(&events), 6);
    assert!(notices(&events).iter().any(|m| m.contains("Call budget reached (2 tool rounds)")), "{:?}", notices(&events));
}

#[tokio::test]
async fn time_spent_waiting_for_approval_does_not_use_up_the_time_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg(tmp.path(), json!({"approval": "on-request", "max_seconds": 0.5}));
    let slow_user: crate::safety::approval::ConfirmCallback = Arc::new(|_| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(800)).await; // the user is away for longer than the whole budget, then approves
            crate::safety::approval::Decision { approved: true, ..Default::default() }
        })
    });
    let mut agent = scripted(cfg, vec![call("shell", json!({"command": "touch made.txt"})), text("done")]).await;
    *agent.session.approval_manager.confirmation_callback.lock().unwrap() = Some(slow_user);
    let events = run(&mut agent, "create made.txt").await;
    assert!(!has_error(&events), "{events:?}");
    assert!(tmp.path().join("made.txt").exists());
    assert!(agent.task.as_ref().unwrap().paused >= 0.8);
}

#[tokio::test]
async fn running_out_of_time_still_produces_an_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({"max_seconds": 0.0001})), vec![text("here is what I know")]).await;
    let events = run(&mut agent, "explain the repo").await;
    assert!(notices(&events).iter().any(|m| m.contains("Time budget reached")), "{:?}", notices(&events));
    assert!(!text_completes(&events).is_empty());
}

#[tokio::test]
async fn silent_rounds_get_a_write_down_checkpoint_and_a_fix_now_nudge() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..16 {
        std::fs::write(tmp.path().join(format!("f{i}.txt")), format!("file {i}")).unwrap(); // different files each round, so the loop detector stays quiet
    }
    let mut script: Vec<_> = (0..16).map(|i| call_as("read_file", json!({"path": format!("f{i}.txt")}), &format!("c{i}"))).collect();
    script.push(text("done"));
    let mut agent = scripted(cfg(tmp.path(), json!({})), script).await;
    run(&mut agent, "review and fix the files").await;
    let said = user_messages(&agent);
    assert_eq!(said.iter().filter(|c| c.contains("Checkpoint:")).count(), 2); // after rounds 8 and 16
    assert_eq!(said.iter().filter(|c| c.contains("nothing is changed yet")).count(), 1); // once, at round 15
}

#[tokio::test]
async fn a_review_that_argues_with_itself_is_rewritten_without_another_loop_round() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let messy = "high a.txt:1 keys clash. Actually a non-issue. Skip.\nlow a.txt:1 extra work. Acceptable; not a bug.";
    // the one read below has used half of the budget when the answer comes
    let mut agent = scripted(cfg(tmp.path(), json!({"read_call_budget": 2})), vec![call("read_file", json!({"path": "a.txt"})), text(messy), text("high a.txt:1 write can 500 the page -> catch")]).await;
    let events = run(&mut agent, "review a.txt").await;
    let done = text_completes(&events);
    assert_eq!(done.last().unwrap(), "high a.txt:1 write can 500 the page -> catch");
    assert!(!notices(&events).iter().any(|m| m.contains("Call budget")));
}

#[tokio::test]
async fn a_review_stopped_by_the_call_cap_is_asked_for_the_review_not_a_next_step_summary_and_still_gets_the_rewrite() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let messy = "HIGH a.txt:1 two ids collide. Finding retracted; moot.";
    let script = vec![call("read_file", json!({"path": "a.txt"})), text(messy), text("HIGH a.txt:1 a failing write 500s the page -> catch")];
    let mut agent = scripted(cfg(tmp.path(), json!({"read_call_budget": 1})), script).await;
    let events = run(&mut agent, "review a.txt").await;
    let asked = user_messages(&agent);
    assert!(asked.iter().any(|c| c.contains("Answer now with the review itself")));
    assert!(!asked.iter().any(|c| c.contains("next step")));
    assert!(text_completes(&events).last().unwrap().starts_with("HIGH a.txt:1 a failing write"));
}

#[tokio::test]
async fn a_review_near_the_call_cap_is_told_to_verify_its_top_two_candidates() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..4 {
        std::fs::write(tmp.path().join(format!("f{i}.txt")), format!("file {i}")).unwrap();
    }
    let mut script: Vec<_> = (0..3).map(|i| call_as("read_file", json!({"path": format!("f{i}.txt")}), &format!("c{i}"))).collect();
    script.push(text("No issues found"));
    let mut agent = scripted(cfg(tmp.path(), json!({"read_call_budget": 4})), script).await;
    run(&mut agent, "review the files").await;
    assert!(user_messages(&agent).iter().any(|c| c.contains("two candidates most likely to be real defects")));
}

#[tokio::test]
async fn todo_set_replaces_the_list_in_one_call_and_survives_a_clear() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![]).await;
    let todos = agent.session.tool_registry.get("todos").unwrap();
    let res = todos.execute(&inv(tmp.path(), json!({"action": "set", "items": [{"content": "read diff", "done": true}, {"content": "fix bug"}]}))).await;
    assert!(res.success && res.output.contains("2 steps, 1 done"), "{}", res.output);
    agent.remind_todos();
    let said = user_messages(&agent);
    assert!(said.iter().any(|c| c.contains("[x] read diff") && c.contains("[ ] fix bug")));
    agent.remind_todos(); // unchanged list: not repeated
    assert_eq!(user_messages(&agent).iter().filter(|c| c.contains("Task list as last set")).count(), 1);
}

#[tokio::test]
async fn before_tool_hook_exit_2_blocks_the_call_and_after_tool_stderr_reaches_the_model() {
    let tmp = tempfile::tempdir().unwrap();
    let hook = |name: &str, trigger, matcher: &str, command: &str| HookConfig {
        name: name.into(),
        trigger,
        command: Some(command.into()),
        script: None,
        matcher: Some(matcher.into()),
        timeout_sec: 30.0,
        enabled: true,
    };
    let mut c = cfg(tmp.path(), json!({"hooks_enabled": true}));
    c.hooks = vec![hook("nope", HookTrigger::BeforeTool, "write_file", "echo no secrets >&2; exit 2"), hook("lint", HookTrigger::AfterTool, "read_file", "echo 2 lint errors >&2; exit 2")];
    let hs = HookSystem::new(&c);
    assert!(hs.trigger_before_tool("write_file", &json!({})).await.unwrap().contains("no secrets"));
    assert!(hs.trigger_before_tool("read_file", &json!({})).await.is_none()); // matcher skips other tools
    let ok = ToolResult::ok("x");
    assert!(hs.trigger_after_tool("read_file", &json!({}), &ok).await.unwrap().contains("2 lint errors"));
}

#[tokio::test]
async fn todos_complete_by_the_id_the_model_gave_in_set_or_by_a_unique_phrase() {
    let tmp = tempfile::tempdir().unwrap();
    let t = TodosTool::default();
    t.execute(&inv(tmp.path(), json!({"action": "set", "items": [{"id": "inspect", "content": "Map the nav links"}, {"content": "Run lint"}]}))).await;
    assert!(t.execute(&inv(tmp.path(), json!({"action": "complete", "id": "inspect"}))).await.success);
    assert!(t.execute(&inv(tmp.path(), json!({"action": "complete", "id": "lint"}))).await.success); // unique phrase of the step
    assert!(!t.execute(&inv(tmp.path(), json!({"action": "complete", "id": "nope"}))).await.success);
}

#[tokio::test]
async fn graph_tool_answers_importers_and_blast_from_the_working_tree_through_ts_aliases() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    std::fs::create_dir_all(d.join("src/components")).unwrap();
    std::fs::create_dir_all(d.join("src/app")).unwrap();
    std::fs::write(d.join("tsconfig.json"), "{ // c\n \"compilerOptions\": {\"baseUrl\": \".\", \"paths\": {\"@/*\": [\"src/*\"],},},}").unwrap();
    std::fs::write(d.join("src/components/Nav.tsx"), "export const Nav = 1\n").unwrap();
    std::fs::write(d.join("src/app/page.tsx"), "import { Nav } from \"@/components/Nav\"\n").unwrap();
    std::fs::write(d.join("src/app/page.test.tsx"), "import P from \"./page\"\n").unwrap();
    git_init(d);
    assert!(std::process::Command::new("git").args(["add", "-A"]).current_dir(d).status().unwrap().success());
    let tool = GraphTool::new(ConfigHandle::new(cfg(d, json!({}))));
    let go = |p: Value| {
        let (tool, d) = (&tool, d.to_path_buf());
        async move { tool.execute(&inv(&d, p)).await.output }
    };
    assert!(go(json!({"action": "importers", "name": "Nav.tsx"})).await.contains("src/app/page.tsx"));
    let out = go(json!({"action": "blast", "name": "src/components/Nav.tsx"})).await;
    assert!(out.contains("page.tsx") && out.contains("page.test.tsx"), "{out}");
    assert!(go(json!({"action": "tests", "name": "Nav.tsx"})).await.contains("page.test.tsx"));
}

#[test]
fn broad_requests_are_recognised() {
    use crate::agent::scope::is_broad;
    for yes in ["fix all such issues in the whole app", "update every button", "rename it everywhere", "check each component for this"] {
        assert!(is_broad(yes), "{yes}");
    }
    for no in ["fix the login bug in auth.ts", "add a button to the nav", "explain how the cache works"] {
        assert!(!is_broad(no), "{no}");
    }
}

#[tokio::test]
async fn a_sweep_with_open_steps_is_sent_back_instead_of_answered() {
    let tmp = tempfile::tempdir().unwrap();
    let items = json!([{"id": "a", "content": "fix Nav", "done": true}, {"id": "b", "content": "fix Footer", "done": false}]);
    let done = json!([{"id": "a", "content": "fix Nav", "done": true}, {"id": "b", "content": "fix Footer", "done": true}]);
    let script = vec![
        call_as("todos", json!({"action": "set", "items": items}), "c1"),
        text("all fixed"), // premature: Footer is still open
        call_as("todos", json!({"action": "set", "items": done}), "c2"),
        text("really done"),
    ];
    let mut agent = scripted(cfg(tmp.path(), json!({})), script).await;
    let events = run(&mut agent, "fix the disabled state on all buttons in the whole app").await;
    let sent = requests(&agent);
    assert_eq!(sent.len(), 4);
    let users: Vec<String> = sent[2].iter().filter(|m| m["role"] == "user").map(|m| m["content"].as_str().unwrap_or("").to_string()).collect();
    assert!(users.join(" ").contains("still open") && serde_json::to_string(&sent[2]).unwrap().contains("fix Footer"));
    assert!(text_completes(&events).iter().any(|t| t == "really done"));
}

#[test]
fn interim_text_shrinks_to_one_short_line() {
    use crate::agent::brevity::interim_line;
    let notes = "Working notes before editing:\n\n- `a.tsx`: verified the CTA is a plain Link\n- `b.tsx:3`: state-only guard";
    assert_eq!(interim_line(notes), "Working notes before editing:");
    let long = interim_line(&format!("- {}", "word ".repeat(60)));
    assert!(long.ends_with('…') && long.chars().count() <= 142, "{long}");
    assert_eq!(interim_line(""), "");
}

#[tokio::test]
async fn text_before_a_tool_call_is_shown_as_one_line_but_a_final_answer_in_full() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let notes = "Working notes:\n- one\n- two\n- three";
    let mixed: Vec<StreamEvent> = text(notes).into_iter().take(1).chain(call("read_file", json!({"path": "a.txt"}))).collect();
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![mixed, text("Fixed it.\n- a.txt:1  done")]).await;
    let events = run(&mut agent, "go").await;
    let shown: String = events.iter().filter_map(|e| if let AgentEvent::TextDelta(c) = e { Some(c.as_str()) } else { None }).collect();
    assert_eq!(shown, "Working notes:Fixed it.\n- a.txt:1  done");
    // history keeps the full notes
    assert!(requests(&agent)[1].iter().any(|m| m["role"] == "assistant" && m["content"] == notes));
}

/// Never answers: a scout that is still "looking" when Ctrl+C arrives.
struct HangingScout;

#[async_trait]
impl Tool for HangingScout {
    fn name(&self) -> &str {
        "subagent_investigator"
    }
    fn description(&self) -> String {
        "scout".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn schema(&self) -> Value {
        schema_of(json!({"goal": {"type": "string"}}), &["goal"])
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn execute(&self, _inv: &ToolInvocation) -> ToolResult {
        tokio::time::sleep(Duration::from_secs(60)).await;
        ToolResult::ok("never")
    }
}

#[tokio::test]
async fn a_request_interrupted_during_the_review_is_kept_for_pls_cont() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({"pi_mode": false, "review_pipeline": false})), vec![]).await;
    agent.session.tool_registry.register(Arc::new(HangingScout));
    let request = "Investigate why the event description loses blank lines when a section exists in the page";
    // the run is parked in the scout, before the request ever reached the history; Ctrl+C drops it there
    let parked = tokio::time::timeout(Duration::from_millis(300), run(&mut agent, request)).await;
    assert!(parked.is_err(), "the run should still be waiting for the scout");
    agent.on_interrupt();
    let kept = user_messages(&agent);
    assert!(kept.len() == 1 && kept[0].starts_with(request) && kept[0].contains("Interrupted by the user"), "{kept:?}");
    agent.on_interrupt(); // a second Ctrl+C does not add it twice
    assert_eq!(user_messages(&agent).len(), 1);
}

#[tokio::test]
async fn review_with_one_single_agent_skips_the_parallel_reviewers() {
    use crate::agent::routing_policy::one_agent;
    for asked in ["pls review the last two commits with one single agent", "review it with a single reviewer", "review without the pipeline"] {
        assert!(one_agent().is_match(asked), "{asked}");
    }
    assert!(!one_agent().is_match("review the last two commits"));

    let tmp = tempfile::tempdir().unwrap();
    // if the pipeline did run it would start agents of its own; point those at a closed local port so nothing leaves the machine
    let mut c = cfg(tmp.path(), json!({"review_pipeline": true}));
    c.model.name = "closed:m".into();
    c.providers.insert("closed".into(), crate::config::ProviderConfig { base_url: Some("http://127.0.0.1:1/v1".into()), protocol: Some("openai".into()), ..Default::default() });
    let mut agent = scripted(c, vec![text("No issues found.")]).await;
    let events = run(&mut agent, "pls review the last two commits with one single agent").await;
    assert!(notices(&events).iter().any(|m| m.contains("one agent, as asked")), "{:?}", notices(&events));
}

#[tokio::test]
async fn a_cut_off_tool_call_is_not_run_and_the_model_is_told_why() {
    let tmp = tempfile::tempdir().unwrap();
    let cut = vec![
        StreamEvent::ToolCallComplete(ToolCall { call_id: "c1".into(), name: "write_file".into(), arguments: json!({"path": "big.py", "content": "def half("}).as_object().unwrap().clone() }),
        StreamEvent::MessageComplete { finish_reason: Some("length".into()), usage: None, reasoning: None, text: None },
    ];
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![cut, text("done")]).await;
    run(&mut agent, "go").await;
    assert!(!tmp.path().join("big.py").exists()); // half a file never reaches the disk
    let result = tool_texts(&requests(&agent)[1]);
    assert!(result[0].contains("output token limit"), "{result:?}");
}

#[tokio::test]
async fn a_cut_off_answer_is_asked_to_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let cut = vec![StreamEvent::TextDelta("The three causes are: one, two".into()), StreamEvent::MessageComplete { finish_reason: Some("length".into()), usage: None, reasoning: None, text: None }];
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![cut, text(", three.")]).await;
    run(&mut agent, "go").await;
    assert!(last_content(&requests(&agent)[1]).contains("cut off"));
}

#[tokio::test]
async fn a_claude_code_style_call_still_runs() {
    // `Read` with file_path is how Claude Code spells read_file(path); open models trained on its transcripts send it
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hello there").unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![call("Read", json!({"file_path": "a.txt"})), text("done")]).await;
    let events = run(&mut agent, "go").await;
    assert!(completed(&events)[0].1.success);
    assert!(tool_texts(&requests(&agent)[1])[0].contains("hello there"));
}
