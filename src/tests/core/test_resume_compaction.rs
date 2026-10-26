//! Ported from tests/test_resume_compaction.py: compact a run on a small repo, save it, resume it, compact again:
//! nothing from before the first compaction may be lost along the way.
//! The Python tests used hand-made summariser classes; here the compactor talks to a real `LLMClient` whose replies are
//! scripted, and the requests it logged tell us what the summariser was asked.
use crate::client::llm_client::LLMClient;
use crate::client::response::StreamEvent;
use crate::config::{Config, ConfigHandle};
use crate::context::compaction::{fork_prompt, ChatCompactor};
use crate::context::ContextManager;
use crate::util::archive::ResultStore;
use crate::util::trace::Tracer;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const GOOD: &str = "## ORIGINAL GOAL\nfix paging\n## COMPLETED ACTIONS\nread f1\n## NEXT STEP\nedit f2";

fn add_pair(cm: &mut ContextManager, i: usize, name: &str, path: &str) {
    let tid = format!("t{i}");
    cm.add_user_message(&format!("step {i} {}", "x".repeat(800)));
    cm.add_assistant_message(Some(""), Some(vec![json!({"id": tid, "type": "function", "function": {"name": name, "arguments": json!({"path": path}).to_string()}})]), None, None);
    cm.add_tool_result(&tid, &format!("ok {}", "y".repeat(800)));
    cm.add_assistant_message(Some(&format!("done {i}")), None, None, None);
}

/// Same replay the app does when it resumes a saved session.
fn resume(cfg: &ConfigHandle, messages: &[Value]) -> ContextManager {
    let mut cm = ContextManager::new(cfg.clone(), String::new());
    for m in messages {
        match m["role"].as_str() {
            Some("user") => cm.add_user_message(m["content"].as_str().unwrap_or("")),
            Some("assistant") => cm.add_assistant_message(Some(m["content"].as_str().unwrap_or("")), m["tool_calls"].as_array().cloned(), None, None),
            Some("tool") => cm.add_tool_result(m["tool_call_id"].as_str().unwrap_or(""), m["content"].as_str().unwrap_or("")),
            _ => {}
        }
    }
    cm
}

fn config(dir: &std::path::Path) -> ConfigHandle {
    let mut c: Config = super::cfg(dir, json!({}));
    c.keep_recent_tokens = 600;
    ConfigHandle::new(c)
}

/// A compactor whose model replies come from `replies`; the client is returned to read what it was asked.
fn compactor(cfg: &ConfigHandle, replies: Vec<Vec<StreamEvent>>) -> (ChatCompactor, Arc<LLMClient>) {
    let client = Arc::new(LLMClient::new(cfg.clone(), Arc::new(Tracer::new("t", None))));
    *client.script.lock().unwrap() = Some(replies.into());
    (ChatCompactor::new(client.clone()), client)
}

fn reply(text: &str, finish: Option<&str>) -> Vec<StreamEvent> {
    vec![StreamEvent::MessageComplete { finish_reason: finish.map(String::from), usage: None, reasoning: None, text: Some(text.into()) }]
}

fn last_len(req: &[Value]) -> usize {
    req.last().unwrap()["content"].as_str().unwrap().len()
}

#[tokio::test]
async fn second_compaction_after_resume_keeps_the_first_summary_and_edited_files() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut cm = ContextManager::new(cfg.clone(), String::new());
    add_pair(&mut cm, 0, "edit", "app/main.py");
    for i in 1..6 {
        add_pair(&mut cm, i, "read_file", &format!("app/f{i}.py"));
    }
    let long_summary = format!("## ORIGINAL GOAL\nFix the login bug in app/main.py. {}\nMARKER-END", "detail ".repeat(400));
    let cut = cm.find_cut(600);
    cm.apply_compaction(&long_summary, cut, None, None);

    let mut resumed = resume(&cfg, &cm.get_messages());
    for i in 6..10 {
        add_pair(&mut resumed, i, "read_file", &format!("app/g{i}.py"));
    }
    // the spy: every reply is empty, so the compactor keeps trying and each request shows what it was asked
    let (comp, client) = compactor(&cfg, vec![vec![]; 6]);
    comp.compress(&Mutex::new(resumed), 600, None).await;
    let reqs = client.requests.lock().unwrap();
    let prompt = reqs[0].last().unwrap()["content"].as_str().unwrap().to_string();
    assert!(prompt.contains("MARKER-END"), "the first summary was cut short when folded into the second");
    assert_eq!(prompt.matches("MARKER-END").count(), 1, "the summary should be passed once, not also as a message");
    assert!(prompt.contains("app/main.py"));
    assert!(prompt.contains("Files modified so far: app/main.py"));
}

#[tokio::test]
async fn compaction_that_overflows_drops_the_oldest_messages_and_asks_again() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut cm = ContextManager::new(cfg.clone(), String::new());
    for i in 0..10 {
        add_pair(&mut cm, i, "read_file", &format!("app/f{i}.py"));
    }
    let (comp, client) = compactor(&cfg, vec![vec![StreamEvent::Error("prompt is too long".into())], reply(GOOD, None)]);
    let (summary, _, cut) = comp.compress(&Mutex::new(cm), 600, None).await;
    assert_eq!(summary.as_deref(), Some(GOOD));
    assert!(cut > 0);
    let reqs = client.requests.lock().unwrap();
    assert!(last_len(&reqs[1]) < last_len(&reqs[0]));
}

#[tokio::test]
async fn a_summary_cut_off_or_without_its_sections_is_not_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut cm = ContextManager::new(cfg.clone(), String::new());
    for i in 0..10 {
        add_pair(&mut cm, i, "read_file", &format!("app/f{i}.py"));
    }
    let (comp, client) = compactor(&cfg, vec![reply(GOOD, Some("length")), reply("Sure! The user wants paging fixed.", Some("stop")), reply(GOOD, Some("stop"))]);
    let (summary, _, _) = comp.compress(&Mutex::new(cm), 600, None).await;
    assert_eq!(summary.as_deref(), Some(GOOD));
    assert!(client.script.lock().unwrap().as_ref().unwrap().is_empty()); // the half summary and the chatty one were both asked again
}

#[test]
fn resumed_archive_keeps_old_ids_and_never_overwrites_them() {
    let tmp = tempfile::tempdir().unwrap();
    let old = ResultStore::new(Some(tmp.path().join("s1")));
    old.put("first");
    old.put("second");
    let fresh = ResultStore::new(Some(tmp.path().join("throwaway")));
    fresh.move_to(&tmp.path().join("s1"));
    assert_eq!(fresh.get("r2").as_deref(), Some("second")); // a cleared result from before the resume is still recallable
    assert_eq!(fresh.put("third"), "r3");
    assert_eq!(fresh.get("r1").as_deref(), Some("first"));
}

/// Python also checked that the tool list and the role name reached the model call. The test client only logs the
/// messages, so here the fork is recognised by its prefix and its closing instruction instead.
#[tokio::test]
async fn the_summary_is_asked_on_the_cached_prefix_and_falls_back_to_the_trimmed_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let mut cm = ContextManager::new(cfg.clone(), "SYSTEM".into());
    for i in 0..10 {
        add_pair(&mut cm, i, "read_file", &format!("app/f{i}.py"));
    }
    let tools = vec![json!({"name": "read_file"})];
    let cm = Mutex::new(cm);
    let (comp, client) = compactor(&cfg, vec![reply(GOOD, None)]);
    let (summary, _, cut) = comp.compress(&cm, 600, Some((tools.clone(), "strong".into()))).await;
    {
        let reqs = client.requests.lock().unwrap();
        let sent = &reqs[0];
        assert_eq!(summary.as_deref(), Some(GOOD));
        assert_eq!(sent[..sent.len() - 1], cm.lock().unwrap().get_messages()[..cut + 1]); // byte-for-byte the prefix the loop sent
        assert_eq!(sent.last().unwrap()["content"], fork_prompt());
    }

    client.requests.lock().unwrap().clear();
    *client.script.lock().unwrap() = Some(vec![reply("no headings here", None), reply(GOOD, None)].into());
    let (summary, _, _) = comp.compress(&cm, 600, Some((tools, "strong".into()))).await;
    assert_eq!(summary.as_deref(), Some(GOOD));
    let reqs = client.requests.lock().unwrap();
    // the fork failed, the trimmed copy did the job: its own system prompt and one flattened history message
    assert_eq!(reqs[1].len(), 2);
    assert_eq!(reqs[1][0]["role"], "system");
    assert!(reqs[1][1]["content"].as_str().unwrap().starts_with("Here is the conversation"));
}
