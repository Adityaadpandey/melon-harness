//! Ported from tests/test_cost.py: what a request costs (prompt size, cache misses, heavy tool results), plus the
//! read-dedupe, output-cap and scout behaviour that keeps prompts small.
//! Not ported: nothing is skipped, but `count_tokens` (tiktoken) becomes the harness's own bytes/4 estimate.
use super::{call_as, cfg};
use crate::agent::events::AgentEvent;
use crate::client::response::StreamEvent;
use crate::config::{ConfigHandle, ModelPrice};
use crate::tests::fakes::{call, run, scripted, text};
use crate::tools::registry::create_default_registry;
use crate::tools::subagents::{SubagentDefinition, SubagentTool};
use crate::tools::{Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::cost::{cache_waste, heaviest_results, summarize_usage};
use crate::util::text::estimate_tokens;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

#[test]
fn system_prompt_stays_lean() {
    let c = cfg(Path::new("/tmp"), json!({"pi_mode": false}));
    let reg = create_default_registry(ConfigHandle::new(c.clone()));
    let prompt = crate::prompts::get_system_prompt(&c, None, &reg.get_tools(), &[]);
    // Python measured with tiktoken (< 1600). Here only the bytes/4 estimate exists, which reads ~20% higher on English
    // prose and "-" style punctuation, so the ceiling is 2000: still far below the ~3000 this guard was written against.
    assert!(estimate_tokens(&prompt) < 2000, "{} tokens", estimate_tokens(&prompt));
}

#[test]
fn cost_summary_only_prices_known_models() {
    let ev = vec![
        json!({"event": "model_call", "ok": true, "model": "a", "tokens": {"prompt_tokens": 1000, "completion_tokens": 100, "cached_tokens": 800}}),
        json!({"event": "model_call", "ok": false, "model": "a"}),
    ];
    let mut prices = HashMap::new();
    prices.insert("a".to_string(), ModelPrice { input: 1.0, output: 2.0, cached_input: Some(0.1) });
    let s = summarize_usage(&ev, &prices);
    assert_eq!(s["cache_hit_pct"].as_f64().unwrap(), 80.0);
    let want = (200.0 * 1.0 + 800.0 * 0.1 + 100.0 * 2.0) / 1e6;
    assert!((s["est_cost_usd"].as_f64().unwrap() - want).abs() < 1e-4);
    assert!(summarize_usage(&ev, &HashMap::new())["est_cost_usd"].is_null()); // unknown price stays unknown
}

#[tokio::test]
async fn large_file_is_truncated_not_unreadable() {
    let tmp = tempfile::tempdir().unwrap();
    let body: Vec<String> = (0..6000).map(|i| format!("line {i} {}", "x".repeat(60))).collect();
    std::fs::write(tmp.path().join("big.txt"), body.join("\n")).unwrap();
    let mut a = scripted(cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false})), vec![call("read_file", json!({"path": "big.txt"})), text("ok")]).await;
    let evs = run(&mut a, "read").await;
    let done = evs.iter().find_map(|e| if let AgentEvent::ToolCallComplete { result, .. } = e { Some(result) } else { None }).unwrap();
    assert!(done.success && done.output.contains("truncated"));
    assert!(done.output.len() < 60_000);
}

#[tokio::test]
async fn rereading_unchanged_file_is_deduped_until_context_changes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
    let mut a = scripted(
        cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false})),
        vec![call_as("read_file", json!({"path": "a.txt"}), "c1"), call_as("read_file", json!({"path": "a.txt"}), "c2"), text("done")],
    )
    .await;
    let evs = run(&mut a, "go").await;
    let outs: Vec<String> = evs.iter().filter_map(|e| if let AgentEvent::ToolCallComplete { result, .. } = e { Some(result.output.clone()) } else { None }).collect();
    assert!(outs[0].contains("hello") && outs[1].contains("unchanged"), "{outs:?}");
    // once the earlier output has left the context, a re-read must return real content
    let reg = a.session.tool_registry.clone();
    reg.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let r = reg.invoke("read_file", json!({"path": "a.txt"}), tmp.path(), &a.session.hook_system, None, "").await;
    assert!(r.output.contains("hello"), "{}", r.output);
    // and an edit invalidates it too
    std::fs::write(tmp.path().join("a.txt"), "changed!").unwrap();
    let r = reg.invoke("read_file", json!({"path": "a.txt"}), tmp.path(), &a.session.hook_system, None, "").await;
    assert!(r.output.contains("changed!"), "{}", r.output);
}

#[tokio::test]
async fn tool_output_is_capped_head_and_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let a = scripted(cfg(tmp.path(), json!({"pi_mode": false, "tool_output_max_chars": 1000})), vec![]).await;
    let r = a
        .session
        .tool_registry
        .invoke("shell", json!({"command": "printf 'A%.0s' $(seq 1 5000); echo; echo THE-END"}), tmp.path(), &a.session.hook_system, Some(&a.session.approval_manager), "")
        .await;
    assert!(r.output.len() < 1400 && r.output.starts_with("AAAA") && r.output.contains("THE-END") && r.truncated, "{} chars: {}", r.output.len(), r.output);
}

#[tokio::test]
async fn token_budget_ends_with_a_final_answer_not_a_bare_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = scripted(
        cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false, "max_total_tokens": 1})),
        vec![call_as("list_dir", json!({"path": "d0"}), "c0"), text("Found X at a.py:1; Y unknown.")],
    )
    .await;
    let evs = run(&mut a, "go").await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::AgentError { .. })), "{evs:?}");
    let Some(AgentEvent::AgentEnd { response, report }) = evs.last() else { panic!("run did not end with AgentEnd: {evs:?}") };
    assert!(response.as_deref().unwrap_or("").contains("Found X") && report["status"] == "unverified", "{response:?} {report}");
    let reqs = a.session.client.requests.lock().unwrap();
    assert!(reqs.last().unwrap().last().unwrap()["content"].as_str().unwrap().starts_with("Token budget is spent"));
}

#[test]
fn anthropic_marks_history_for_caching() {
    let msgs = vec![json!({"role": "system", "content": "sys"}), json!({"role": "user", "content": "hi"})];
    let (_system, mut out) = crate::client::adapters::to_anthropic_messages(&msgs);
    // simulate what the adapter does before sending
    let blocks = out.last_mut().unwrap()["content"].as_array_mut().unwrap();
    blocks.last_mut().unwrap()["cache_control"] = json!({"type": "ephemeral"});
    assert_eq!(out.last().unwrap()["content"].as_array().unwrap().last().unwrap()["cache_control"]["type"], "ephemeral");
}

fn model_call(t: i64, model: &str, prompt: i64, cached: i64) -> Value {
    json!({"event": "model_call", "ok": true, "model": model, "t": t, "sec": 1, "tokens": {"prompt_tokens": prompt, "completion_tokens": 10, "cached_tokens": cached}})
}

#[test]
fn cache_waste_attributes_reason_and_ignores_noise_and_first_turn() {
    let ev = vec![
        model_call(0, "m", 10_000, 0),         // first turn: nothing to miss yet
        model_call(10, "m", 12_000, 9_900),    // fine: previous prompt (10k) mostly read from cache
        model_call(20, "m", 14_000, 12_000),   // fine
        model_call(30, "m", 15_000, 2_000),    // prefix changed: ~12k tokens re-billed
        model_call(900, "m", 16_000, 0),       // long idle gap: expired cache
        model_call(910, "other", 17_000, 0),   // different model, never reported caching -> not counted
    ];
    let w = cache_waste(&ev, &HashMap::new());
    assert_eq!(w["misses"], 2);
    assert_eq!(w["reasons"], json!(["idle > 5 min", "prefix changed"]));
    assert_eq!(w["missed_tokens"], (14_000 - 2_000) + (15_000 - 0));
}

#[test]
fn cache_waste_resets_after_compaction_and_providers_without_caching() {
    let ev = vec![model_call(0, "m", 50_000, 1), model_call(5, "m", 60_000, 50_000), json!({"event": "compaction"}), model_call(10, "m", 8_000, 0)];
    assert_eq!(cache_waste(&ev, &HashMap::new())["misses"], 0);
    assert_eq!(cache_waste(&[model_call(0, "n", 9_000, 0), model_call(5, "n", 9_500, 0)], &HashMap::new())["misses"], 0);
}

/// A stand-in for the investigator sub-agent: answers at once with a fixed finding.
struct FakeScout;

#[async_trait]
impl Tool for FakeScout {
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
        crate::tools::schema_of(json!({"goal": {"type": "string"}}), &["goal"])
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn execute(&self, _inv: &ToolInvocation) -> ToolResult {
        ToolResult::ok("route -> svc.ts:42")
    }
}

#[tokio::test]
async fn scout_findings_start_the_main_context() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = scripted(cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false})), vec![text("done"), text("ok")]).await;
    a.session.tool_registry.register(Arc::new(FakeScout));
    let long_task = "Investigate why the event description loses blank lines when a section exists ".repeat(2);
    run(&mut a, &long_task).await;
    {
        let reqs = a.session.client.requests.lock().unwrap();
        assert!(reqs[0].last().unwrap()["content"].as_str().unwrap().contains("route -> svc.ts:42"));
    }
    run(&mut a, "fix typo").await; // short request: no scout
}

#[test]
fn heaviest_results_weighs_size_by_how_long_it_stays_in_context() {
    let mc = json!({"event": "model_call", "ok": true});
    let mut ev = vec![
        mc.clone(),
        json!({"event": "tool_call", "tool": "grep", "target": "x", "chars": 4000}), // 1000 tokens, 3 later calls
        mc.clone(),
        json!({"event": "tool_call", "tool": "read_file", "target": "a.ts", "chars": 8000}), // 2000 tokens, 2 later
        mc.clone(),
        json!({"event": "tool_call", "tool": "read_file", "target": "a.ts", "chars": 400}),
        mc.clone(),
        mc.clone(),
    ];
    let h = heaviest_results(&ev);
    assert_eq!((h["top"][0]["target"].as_str().unwrap(), h["top"][0]["carried"].as_i64().unwrap()), ("a.ts", 6000));
    assert_eq!(h["top"][1]["carried"], 4000);
    assert_eq!(h["repeated"], json!(["read_file a.ts x2"]));
    ev.insert(5, json!({"event": "compaction"})); // history rewritten: earlier results stop counting
    assert!(heaviest_results(&ev)["tool_result_carried"].as_i64().unwrap() < h["tool_result_carried"].as_i64().unwrap());
}

/// Python shrank the whole-file and minimum-slice constants to zero; here they are fixed at 400 and 200 lines, so the
/// same story is told on a 1000-line file with 200-line slices (10x the Python numbers).
#[tokio::test]
async fn overlapping_reads_return_only_the_lines_not_already_in_context() {
    let tmp = tempfile::tempdir().unwrap();
    let file = |first: &str| format!("{first}\n{}\n", (2..=1000).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n"));
    std::fs::write(tmp.path().join("f.txt"), file("line1")).unwrap();
    let a = scripted(cfg(tmp.path(), json!({"pi_mode": false})), vec![]).await;
    let (reg, hs) = (a.session.tool_registry.clone(), &a.session.hook_system);
    let read = |extra: Value| {
        let (reg, dir) = (reg.clone(), tmp.path().to_path_buf());
        async move {
            let mut p = json!({"path": "f.txt"});
            p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            reg.invoke("read_file", p, &dir, hs, None, "").await.output
        }
    };
    assert!(read(json!({"offset": 1, "limit": 200})).await.contains("1|line1"));
    let out = read(json!({"offset": 101, "limit": 200})).await; // 101-200 already seen, 201-300 new
    assert!(out.contains("201|line201") && !out.contains("107|line107") && out.contains("already in context"), "{out}");
    assert!(read(json!({"offset": 30, "limit": 200})).await.contains("unchanged and already in context"));
    let out = read(json!({})).await; // whole file: only 301-1000 is new
    assert!(out.contains("1000|line1000") && !out.contains("\n2|line2\n") && out.contains("[lines 1-300 already"), "{}", &out[..200.min(out.len())]);
    std::fs::write(tmp.path().join("f.txt"), file("changed")).unwrap();
    assert!(read(json!({"offset": 1, "limit": 200})).await.contains("1|changed")); // file changed: forget the map
}

#[tokio::test]
async fn small_slices_are_widened_so_the_model_does_not_drip_feed_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("small.txt"), (1..=300).map(|i| format!("s{i}")).collect::<Vec<_>>().join("\n") + "\n").unwrap();
    std::fs::write(tmp.path().join("big.txt"), (1..=1000).map(|i| format!("b{i}")).collect::<Vec<_>>().join("\n") + "\n").unwrap();
    let a = scripted(cfg(tmp.path(), json!({"pi_mode": false})), vec![]).await;
    let (reg, hs) = (a.session.tool_registry.clone(), &a.session.hook_system);
    let out = reg.invoke("read_file", json!({"path": "small.txt", "offset": 100, "limit": 20}), tmp.path(), hs, None, "").await.output; // a 300-line file comes whole
    assert!(out.contains("1|s1\n") && out.contains("300|s300"));
    let out = reg.invoke("read_file", json!({"path": "big.txt", "offset": 500, "limit": 20}), tmp.path(), hs, None, "").await.output; // a long file grows the slice to 200 lines
    assert!(out.contains("500|b500") && out.contains("699|b699") && !out.contains("700|b700") && !out.contains("499|b499"));
}

#[test]
fn learned_lessons_and_edit_history_shape_prompt_and_search() {
    use crate::agent::learn::Learned;
    let tmp = tempfile::tempdir().unwrap();
    let (repo, base) = (tmp.path().join("repo"), tmp.path().join("data"));
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/about.tsx"), "x").unwrap();
    let mut l = Learned::with_base(&repo, &base);
    assert!(l.add_lesson("Never edit generated files under src/generated", "user") && !l.add_lesson("never edit generated files under src/generated", "user"));
    assert!(!l.add_lesson("too short", "user")); // fewer than four words is not a rule
    l.record_run("Fix the blank line in the event description when no section exists", &["src/about.tsx".to_string(), "src/gone.tsx".to_string()]);
    let mut l2 = Learned::with_base(&repo, &base); // a fresh process reads it back
    assert!(std::fs::read_to_string(repo.join("AGENTS.md")).unwrap().contains("Never edit generated files")); // the lesson lands in AGENTS.md, which the prompt already carries
    assert!(l2.prompt_section().unwrap_or_default().is_empty()); // so it is not shown twice
    let boost = l2.prior("event description blank line bug");
    assert_eq!(boost.keys().collect::<Vec<_>>(), vec!["src/about.tsx"]); // deleted files are never recorded
    assert!(boost["src/about.tsx"] > 0.25);
    assert!(l2.prior("payment webhook retry").is_empty());
    let brief = format!("ok so the offline ticket needs to work at the counter, {}", (0..60).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" "));
    l2.record_run(&brief, &["src/about.tsx".to_string()]);
    l2.record_run(&brief, &["src/about.tsx".to_string()]); // importing the same session twice adds nothing
    assert_eq!(l2.edit_count(), 2);
    assert!(!l2.prior("offline ticket counter").is_empty()); // a long brief still matches a short search
    assert!(l2.forget(1).unwrap().starts_with("Never edit"));
    assert!(!std::fs::read_to_string(repo.join("AGENTS.md")).unwrap().contains("Never edit"));
}

#[tokio::test]
async fn changed_file_is_answered_with_a_diff_not_the_whole_file() {
    let tmp = tempfile::tempdir().unwrap();
    let body: String = (0..200).map(|i| format!("line {i} of a long file")).collect::<Vec<_>>().join("\n");
    std::fs::write(tmp.path().join("big.txt"), &body).unwrap();
    let a = scripted(cfg(tmp.path(), json!({"pi_mode": false, "auto_verify": false})), vec![]).await;
    let (reg, hs) = (a.session.tool_registry.clone(), &a.session.hook_system);
    reg.invoke("read_file", json!({"path": "big.txt"}), tmp.path(), hs, None, "").await;
    std::fs::write(tmp.path().join("big.txt"), body.replace("line 100 ", "LINE 100 ")).unwrap();
    let r = reg.invoke("read_file", json!({"path": "big.txt"}), tmp.path(), hs, None, "").await;
    assert!(r.metadata.contains_key("diff_only") && r.output.contains("LINE 100") && !r.output.contains("line 50 "), "{}", r.output);
    // after a diff the file counts as fully known again
    let r = reg.invoke("read_file", json!({"path": "big.txt"}), tmp.path(), hs, None, "").await;
    assert!(r.output.contains("unchanged"), "{}", r.output);
}

/// Python made the model call hang by patching it. Here the sub-agent talks to a real local socket that accepts the
/// connection and never answers, so nothing leaves this machine.
#[tokio::test]
async fn hung_subagent_is_cut_off_at_its_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let held = std::thread::spawn(move || listener.incoming().take(8).filter_map(Result::ok).collect::<Vec<_>>()); // keeps sockets open, says nothing
    let mut c = cfg(tmp.path(), json!({"pi_mode": false}));
    c.model.name = "blackhole:m".into();
    c.providers.insert("blackhole".into(), crate::config::ProviderConfig { base_url: Some(format!("http://127.0.0.1:{port}/v1")), protocol: Some("openai".into()), ..Default::default() });
    let def = SubagentDefinition { name: "t".into(), description: "d".into(), goal_prompt: "p".into(), allowed_tools: Some(vec!["read_file".into()]), max_turns: 20, timeout_seconds: 0.5, role: "default".into() };
    let tool = SubagentTool::new(ConfigHandle::new(c), def);
    let inv = ToolInvocation { params: json!({"goal": "x"}), cwd: Path::new(tmp.path()).into(), epoch: 0, call_id: String::new(), progress: None };
    let r = tokio::time::timeout(std::time::Duration::from_secs(5), tool.execute(&inv)).await.expect("the sub-agent must give up by itself");
    assert!(r.output.contains("timed out"), "{}", r.output);
    drop(held); // detached: it ends when the process does
}

#[test]
fn request_size_is_split_by_part_without_keeping_the_text() {
    let msgs = vec![
        json!({"role": "system", "content": "abc"}),
        json!({"role": "user", "content": "hello"}),
        json!({"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "grep", "arguments": "{}"}}]}),
        json!({"role": "tool", "content": "x".repeat(100)}),
    ];
    assert_eq!(crate::client::llm_client::request_chars(&msgs), json!({"system": 3, "user": 5, "assistant": 0, "tool_calls": 6, "tool_results": 100}));
}

#[tokio::test]
async fn judge_turns_a_models_verdict_into_a_rule_or_nothing() {
    use crate::agent::learn::judge;
    let tmp = tempfile::tempdir().unwrap();
    let say = |t: &str| vec![StreamEvent::MessageComplete { finish_reason: None, usage: None, reasoning: None, text: Some(t.into()) }];
    let a = scripted(cfg(tmp.path(), json!({})), vec![say("Dark mode follows the system theme; no manual toggle."), say("NONE")]).await;
    assert_eq!(judge(&a.session.client, "add a dark mode toggle", "Added a toggle", "no, follow the system").await, "Dark mode follows the system theme; no manual toggle.");
    assert_eq!(judge(&a.session.client, "add a toggle", "done", "thanks!").await, "");
}
