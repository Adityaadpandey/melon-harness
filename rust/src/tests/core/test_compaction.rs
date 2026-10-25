//! Ported from tests/test_compaction.py: cutting, folding and pruning the history.
//! Python faked `chat_compactor.compress`; the Rust compactor is a concrete type, so these tests script the summary
//! as a model reply instead (see `summary_turn`), which exercises the real summarising path too.
use super::{call_as, cfg, summary_turn};
use crate::agent::events::AgentEvent;
use crate::client::response::TokenUsage;
use crate::config::ConfigHandle;
use crate::context::ContextManager;
use crate::tests::fakes::{call, run, scripted, text};
use crate::tools::ToolResult;
use crate::util::archive::ResultStore;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

fn read_call(id: &str, path: &str) -> Vec<Value> {
    vec![json!({"id": id, "type": "function", "function": {"name": "read_file", "arguments": json!({"path": path}).to_string()}})]
}

fn grep_call(id: &str) -> Vec<Value> {
    vec![json!({"id": id, "type": "function", "function": {"name": "grep", "arguments": "{}"}})]
}

fn new_cm(dir: &Path, extra: Value) -> ContextManager {
    ContextManager::new(ConfigHandle::new(cfg(dir, extra)), String::new())
}

fn cm_with_history(dir: &Path, n_pairs: usize) -> ContextManager {
    let mut cm = new_cm(dir, json!({"pi_mode": false}));
    for i in 0..n_pairs {
        cm.add_user_message(&format!("task {i} {}", "x".repeat(1000)));
        let tid = format!("t{i}");
        cm.add_assistant_message(Some(""), Some(read_call(&tid, &format!("f{i}.py"))), None, None);
        cm.add_tool_result(&tid, &format!("content {}", "y".repeat(1000)));
        cm.add_assistant_message(Some(&format!("done {i}")), None, None, None);
    }
    cm
}

fn long_history(dir: &Path, extra: Value) -> ContextManager {
    let mut cm = new_cm(dir, extra);
    cm.add_user_message("fix it");
    for i in 0..20 {
        cm.add_assistant_message(None, Some(grep_call(&format!("c{i}"))), None, None);
        cm.add_tool_result(&format!("c{i}"), &format!("result number {i}\n{}", "word ".repeat(3000))); // ~3k tokens each
    }
    cm
}

#[test]
fn cut_never_splits_a_tool_call_from_its_result() {
    let tmp = tempfile::tempdir().unwrap();
    let cm = cm_with_history(tmp.path(), 6);
    for keep in [1, 300, 900, 1500, 5000] {
        let cut = cm.find_cut(keep);
        assert!(cut == cm.messages.len() || cm.messages[cut].role != "tool", "keep {keep} cut {cut}");
    }
    assert_eq!(cm.find_cut(1_000_000_000), 0);
}

#[test]
fn compaction_keeps_the_recent_tail_verbatim_and_tracks_files() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cm = cm_with_history(tmp.path(), 6);
    let tail_before: Vec<String> = cm.messages[cm.messages.len() - 4..].iter().map(|m| m.content.clone()).collect();
    let cut = cm.find_cut(600);
    assert!(cut > 0 && cut < cm.messages.len());
    cm.apply_compaction("SUMMARY TEXT", cut, None, None);
    assert!(cm.messages[0].role == "user" && cm.messages[0].content.contains("SUMMARY TEXT"));
    let tail_after: Vec<String> = cm.messages[cm.messages.len() - 4..].iter().map(|m| m.content.clone()).collect();
    assert_eq!(tail_after, tail_before);
    assert!(cm.summary.as_ref().unwrap().contains("f0.py")); // files read are listed deterministically, not left to the model
    // a second compaction folds into the running summary and keeps accumulating files
    cm.add_user_message("more");
    let cut2 = cm.find_cut(300);
    cm.apply_compaction("SECOND", cut2, None, None);
    let s = cm.summary.as_ref().unwrap();
    assert!(s.starts_with("SECOND") && s.contains("f0.py"));
}

#[test]
fn repair_orphans_closes_unanswered_tool_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cm = new_cm(tmp.path(), json!({"pi_mode": false}));
    cm.add_user_message("go");
    cm.add_assistant_message(
        Some(""),
        Some(vec![json!({"id": "a", "type": "function", "function": {"name": "x", "arguments": "{}"}}), json!({"id": "b", "type": "function", "function": {"name": "y", "arguments": "{}"}})]),
        None,
        None,
    );
    cm.add_tool_result("a", "ok");
    assert_eq!(cm.repair_orphans(), 1);
    let roles: Vec<(String, Option<String>)> = cm.messages.iter().map(|m| (m.role.clone(), m.tool_call_id.clone())).collect();
    assert_eq!(roles, vec![("user".to_string(), None), ("assistant".into(), None), ("tool".into(), Some("a".into())), ("tool".into(), Some("b".into()))]);
}

fn notices(evs: &[AgentEvent]) -> Vec<String> {
    evs.iter().filter_map(|e| if let AgentEvent::Notice(m) = e { Some(m.clone()) } else { None }).collect()
}

#[tokio::test]
async fn agent_compacts_mid_run_and_continues() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let cfg = cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false, "compact_at_tokens": 50, "keep_recent_tokens": 10}));
    // the summary is the first thing asked for, then the run carries on
    let script = vec![summary_turn("FOLDED", TokenUsage { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2, ..Default::default() }), call("read_file", json!({"path": "a.txt"})), call_as("read_file", json!({"path": "a.txt"}), "c2"), text("done")];
    let mut a = scripted(cfg, script).await;
    {
        let mut cm = a.session.context_manager.lock().unwrap();
        for i in 0..6 {
            cm.add_user_message(&format!("earlier request {i} {}", "x".repeat(200)));
            cm.add_assistant_message(Some(&format!("earlier answer {i} {}", "y".repeat(200))), None, None, None);
        }
        cm.latest_usage = TokenUsage { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110, ..Default::default() }; // already over the 50-token limit
    }
    let evs = run(&mut a, "go").await;
    assert!(notices(&evs).iter().any(|m| m.contains("compacted")), "{:?}", notices(&evs));
    let cm = a.session.context_manager.lock().unwrap();
    assert!(cm.messages[0].content.contains("FOLDED"));
}

#[test]
fn pruning_works_inside_one_task_keeps_newest_and_leaves_a_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cm = long_history(tmp.path(), json!({"pi_mode": false}));
    assert!(cm.prune_tool_outputs() > 0);
    let tools: Vec<&crate::context::manager::MessageItem> = cm.messages.iter().filter(|m| m.role == "tool").collect();
    assert!(tools[0].content.starts_with("[Old tool result cleared") && tools[0].content.contains("result number 0"));
    assert!(tools[tools.len() - 1].content.contains("word word") && tools[tools.len() - 2].content.contains("word word")); // newest stay whole
}

#[tokio::test]
async fn failed_compaction_backs_off_instead_of_retrying_every_turn() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hi").unwrap();
    let cfg = cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false, "compact_at_tokens": 50, "keep_recent_tokens": 10}));
    let steps = vec![call_as("read_file", json!({"path": "a.txt"}), "c0"), call_as("read_file", json!({"path": "a.txt"}), "c1"), call_as("read_file", json!({"path": "a.txt"}), "c2"), text("done")];
    let mut a = scripted(cfg, steps).await;
    // one lone request has nothing older to fold, so the summary cannot be made: the failing case
    a.session.context_manager.lock().unwrap().latest_usage = TokenUsage { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110, ..Default::default() };
    let evs = run(&mut a, "go").await;
    let n = notices(&evs);
    assert_eq!(n.iter().filter(|m| m.contains("Could not summarise")).count(), 1, "{n:?}"); // 4 turns, one failed attempt, then back-off
    assert!(n.iter().any(|m| m.contains("pruned")));
}

#[test]
fn a_cleared_result_can_be_recalled_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cm = long_history(tmp.path(), json!({"pi_mode": false}));
    cm.archive = Some(Arc::new(ResultStore::new(Some(tmp.path().join("res")))));
    let original = cm.messages[2].content.clone();
    assert!(cm.prune_tool_outputs() > 0);
    let note = cm.messages[2].content.clone();
    let rid = note.split("recall_result ").nth(1).and_then(|s| s.split_whitespace().next()).unwrap().to_string();
    assert!(rid.starts_with('r') && rid[1..].chars().all(|c| c.is_ascii_digit()), "{note}");
    assert_eq!(cm.archive.as_ref().unwrap().get(&rid).unwrap(), original);
    assert_eq!(ResultStore::new(Some(tmp.path().join("res"))).get(&rid).unwrap(), original); // a later process can read it too
}

#[tokio::test]
async fn big_output_is_cut_visibly_and_the_full_text_is_recallable() {
    let tmp = tempfile::tempdir().unwrap();
    let a = scripted(cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false, "tool_output_max_chars": 1000})), vec![]).await;
    let reg = a.session.tool_registry.clone();
    let out = format!("line one\n{}FAILED test_x: boom\n", "noise\n".repeat(2000));
    let mut res = ToolResult::ok(out);
    reg.cap_output(&mut res, "shell");
    assert!(res.truncated && res.output.contains("FAILED test_x: boom")); // the tail, where failures live, survives
    let rid = res.output.split("full output: ").nth(1).and_then(|s| s.split(|c: char| !c.is_ascii_alphanumeric()).next()).unwrap().to_string();
    let recalled = reg.invoke("recall_result", json!({"id": rid, "offset": 1, "limit": 3}), tmp.path(), &a.session.hook_system, None, "").await;
    assert!(recalled.output.starts_with("line one\nnoise"), "{}", recalled.output);
}

/// Python shrank the pruning thresholds for this one; here they are fixed at 20k tokens, so six real reads of about
/// 7k tokens each are needed before the oldest ones fall outside the protected newest 20k.
#[tokio::test]
async fn pruning_forgets_only_the_files_it_cleared() {
    let tmp = tempfile::tempdir().unwrap();
    let big: String = (0..250).map(|i| format!("line {i} {}", "word ".repeat(20))).collect::<Vec<_>>().join("\n");
    let names = ["a", "b", "c", "d", "e", "f"];
    for n in names {
        std::fs::write(tmp.path().join(format!("{n}.txt")), &big).unwrap();
    }
    let mut agent = scripted(cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false, "tool_output_max_chars": 200_000})), vec![]).await;
    let reg = agent.session.tool_registry.clone();
    for (i, n) in names.iter().enumerate() {
        let (cid, name) = (format!("c{i}"), format!("{n}.txt"));
        agent.session.context_manager.lock().unwrap().add_assistant_message(Some(""), Some(read_call(&cid, &name)), None, None);
        let res = reg.invoke("read_file", json!({"path": name}), tmp.path(), &agent.session.hook_system, None, &cid).await;
        agent.session.context_manager.lock().unwrap().add_tool_result(&cid, &res.output);
    }
    agent.prune(false, true); // the oldest reads are cleared, the newest two are still in context
    let cleared = agent.session.context_manager.lock().unwrap().last_prune.read_paths.clone();
    assert!(cleared.contains(&"a.txt".to_string()) && !cleared.contains(&"f.txt".to_string()) && !cleared.contains(&"e.txt".to_string()), "{cleared:?}");
    let again_f = reg.invoke("read_file", json!({"path": "f.txt"}), tmp.path(), &agent.session.hook_system, None, "c-f").await;
    let again_a = reg.invoke("read_file", json!({"path": "a.txt"}), tmp.path(), &agent.session.hook_system, None, "c-a").await;
    assert!(again_f.output.contains("already in context") && again_f.output.len() < 200, "{}", again_f.output);
    assert!(again_a.output.contains("line 0 "), "{}", &again_a.output[..100.min(again_a.output.len())]); // cleared, so it is sent again in full
}

#[test]
fn cleared_result_names_the_call_that_made_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cm = new_cm(tmp.path(), json!({"pi_mode": true}));
    cm.add_user_message("review it");
    for i in 0..20 {
        cm.add_assistant_message(None, Some(read_call(&format!("c{i}"), &format!("src/a{i}.py"))), None, None);
        cm.add_tool_result(&format!("c{i}"), &"word ".repeat(3000));
    }
    assert!(cm.prune_tool_outputs() > 0);
    let first = cm.messages.iter().find(|m| m.role == "tool").unwrap().content.clone();
    assert!(first.contains("read_file") && first.contains("src/a0.py")); // the model knows what to re-run
}

#[tokio::test]
async fn pruning_waits_until_the_context_is_three_quarters_full() {
    let tmp = tempfile::tempdir().unwrap();
    let mut agent = scripted(cfg(tmp.path(), json!({})), vec![]).await;
    {
        let mut cm = agent.session.context_manager.lock().unwrap();
        for i in 0..8 {
            cm.add_assistant_message(Some(""), Some(grep_call(&format!("c{i}"))), None, None);
            cm.add_tool_result(&format!("c{i}"), &"word ".repeat(20_000));
        }
        cm.set_latest_usage(TokenUsage { prompt_tokens: 90_000, completion_tokens: 10, total_tokens: 90_010, ..Default::default() }); // under prune_at_tokens: left alone
    }
    agent.prune(false, false);
    assert!(!agent.session.context_manager.lock().unwrap().messages.iter().any(|m| m.pruned));
    agent.session.context_manager.lock().unwrap().set_latest_usage(TokenUsage { prompt_tokens: 200_000, completion_tokens: 10, total_tokens: 200_010, ..Default::default() });
    agent.prune(false, false);
    assert!(agent.session.context_manager.lock().unwrap().messages.iter().any(|m| m.pruned));
}

#[test]
fn summarizer_sees_the_whole_user_message_and_todo_plan() {
    let bugs: String = (1..=10).map(|i| format!("{i}. bug number {i} {}", "z".repeat(300))).collect::<Vec<_>>().join("\n");
    let todos = json!({"op": "set", "items": (0..12).map(|i| format!("step {i} {}", "w".repeat(150))).collect::<Vec<_>>()}).to_string();
    let history = vec![
        json!({"role": "user", "content": bugs}),
        json!({"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "todos", "arguments": todos}}]}),
    ];
    let out = crate::context::compaction::format_history(&history);
    assert!(out.contains("10. bug number 10")); // was cut at 1500 chars, losing items 6-10
    assert!(out.contains("step 11")); // was cut at 500 chars, losing the plan after step 4
}

#[test]
fn compacted_messages_are_saved_and_the_summary_points_at_them() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cm = cm_with_history(tmp.path(), 6);
    let log = tmp.path().join("t").join("s.jsonl");
    let cut = cm.find_cut(600);
    cm.apply_compaction("SUMMARY", cut, Some(&log), None);
    let saved: Vec<Value> = std::fs::read_to_string(&log).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(saved[0]["content"].as_str().unwrap().starts_with("task 0") && saved.len() == cut);
    assert!(cm.messages[0].content.contains(&log.to_string_lossy().to_string()));
    // a resumed session still pulls the summary back out with the pointer in the intro
    let mut cm2 = new_cm(tmp.path(), json!({"pi_mode": false}));
    cm2.add_user_message(&cm.messages[0].content);
    assert!(cm2.summary.as_ref().unwrap().starts_with("SUMMARY"));
    // the second compaction appends only new messages, not the summary head
    cm.add_user_message(&format!("more {}", "m".repeat(2000)));
    let cut2 = cm.find_cut(300);
    cm.apply_compaction("SECOND", cut2, Some(&log), None);
    let lines: Vec<Value> = std::fs::read_to_string(&log).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(!lines.iter().any(|l| l["content"].as_str().unwrap_or("").starts_with("# Earlier")));
}

fn summary_usage() -> TokenUsage {
    TokenUsage { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2, ..Default::default() }
}

#[tokio::test]
async fn folder_rules_come_back_after_a_compaction() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("web")).unwrap();
    std::fs::write(tmp.path().join("web/AGENTS.md"), "WEB RULE: ask before restyling").unwrap();
    std::fs::write(tmp.path().join("web/a.ts"), "x").unwrap();
    std::fs::write(tmp.path().join("web/b.ts"), "y").unwrap();
    let cfg = cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false, "keep_recent_tokens": 10}));
    let script = vec![call("read_file", json!({"path": "web/a.ts"})), text("done"), summary_turn("FOLDED", summary_usage()), call_as("read_file", json!({"path": "web/b.ts"}), "c2"), text("done")];
    let mut a = scripted(cfg, script).await;
    run(&mut a, "first").await;
    assert!(a.compact(false).await); // the read that carried the rule is folded into the summary
    run(&mut a, "second").await;
    let cm = a.session.context_manager.lock().unwrap();
    let results: Vec<&String> = cm.messages.iter().filter(|m| m.role == "tool").map(|m| &m.content).collect();
    assert!(results.last().unwrap().contains("WEB RULE"), "{results:?}");
}

#[tokio::test]
async fn files_worked_in_come_back_after_a_compaction_as_they_are_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.ts"), "old text").unwrap();
    // keep_recent_tokens 1: only the last message is kept verbatim, so the read itself is in the folded part
    let cfg = cfg(tmp.path(), json!({"auto_verify": false, "keep_recent_tokens": 1}));
    let mut a = scripted(cfg, vec![call("read_file", json!({"path": "a.ts"})), text("done"), summary_turn("FOLDED", summary_usage())]).await;
    run(&mut a, "first").await;
    std::fs::write(tmp.path().join("a.ts"), "NEW TEXT").unwrap(); // edited since it was read
    assert!(a.compact(false).await);
    let cm = a.session.context_manager.lock().unwrap();
    let last = &cm.messages.last().unwrap().content;
    assert!(last.contains("## a.ts\nNEW TEXT") && !last.contains("old text"), "{last}");
}

#[test]
fn recent_files_are_newest_first_and_skip_what_the_tail_still_has() {
    let tmp = tempfile::tempdir().unwrap();
    let cm = cm_with_history(tmp.path(), 4); // reads f0..f3, four messages per pair
    assert_eq!(cm.recent_files(12, 5), vec!["f2.py", "f1.py", "f0.py"]); // f3 sits in the kept tail
    assert_eq!(cm.recent_files(16, 2), vec!["f3.py", "f2.py"]);
}

#[tokio::test]
async fn after_a_compaction_the_context_size_is_the_new_history_not_the_summary_call() {
    // the summary request itself weighed 20k; reporting that as the context size made /context lie after /compact
    let tmp = tempfile::tempdir().unwrap();
    let cfg = cfg(tmp.path(), json!({"auto_verify": false, "keep_recent_tokens": 1}));
    let big = call("write_file", json!({"path": "a.ts", "content": "x = 1\n".repeat(2000)}));
    let mut a = scripted(cfg, vec![big, text("done"), summary_turn("FOLDED", TokenUsage { prompt_tokens: 19_947, completion_tokens: 2_519, total_tokens: 22_466, ..Default::default() })]).await;
    run(&mut a, "first").await;
    {
        let cm = a.session.context_manager.lock().unwrap();
        let write = cm.messages.iter().find(|m| !m.tool_calls.is_empty()).unwrap();
        assert!(write.token_count > 1000); // the call's arguments are counted, not just its (empty) text
    }
    assert!(a.compact(false).await);
    let cm = a.session.context_manager.lock().unwrap();
    assert_eq!(cm.latest_usage.total_tokens, cm.estimated_tokens() as i64);
    assert_ne!(cm.latest_usage.total_tokens, 22_466);
    assert!(cm.total_usage.completion_tokens >= 2_519); // the summary call is still billed
}
