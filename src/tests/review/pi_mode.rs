//! Ported from tests/test_pi_mode.py
use crate::agent::skills::Skill;
use crate::config::{Config, ConfigHandle};
use crate::context::manager::ContextManager;
use crate::hooks::HookSystem;
use crate::prompts::{get_system_prompt, skill_rows};
use crate::tests::fakes::init;
use crate::tools::builtin::read_file::pi_read;
use crate::tools::builtin::write_file::WriteFileTool;
use crate::tools::registry::{create_default_registry, ToolRegistry};
use crate::tools::{Tool, ToolInvocation, ToolResult};
use serde_json::{json, Value};
use std::path::Path;

/// Python's `Config(cwd=..., **kw)`: built through the same map the loader uses, so pi mode fills in what nobody set.
fn cfg(dir: &Path, extra: Value) -> Config {
    init();
    let mut m = extra.as_object().cloned().unwrap_or_default();
    m.insert("cwd".into(), json!(dir.to_string_lossy()));
    Config::from_map(m).unwrap()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn pi_mode_gives_the_plain_tools_plus_a_step_list_and_keeps_the_read_call_cap() {
    let dir = tempfile::tempdir().unwrap();
    let c = cfg(dir.path(), json!({}));
    assert_eq!(c.allowed_tools.unwrap(), strs(&["read_file", "write_file", "edit", "shell", "grep", "glob", "list_dir", "graph", "todos", "ask_user", "advisor", "subagent_codebase_investigator"]));
    assert!(c.read_call_budget == 12 && !c.check_references && !c.auto_scout);
}

#[test]
fn anything_set_by_hand_wins_over_pi_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let c = cfg(dir.path(), json!({"read_call_budget": 5, "allowed_tools": ["git"]}));
    assert!(c.read_call_budget == 5 && c.allowed_tools == Some(strs(&["git"])));
    assert!(cfg(dir.path(), json!({"pi_mode": false})).allowed_tools.is_none());
}

#[test]
fn pi_mode_compacts_only_near_the_window_and_still_clears_old_results() {
    let dir = tempfile::tempdir().unwrap();
    let mut cm = ContextManager::new(ConfigHandle::new(cfg(dir.path(), json!({}))), String::new());
    cm.context_window = 100_000;
    assert_eq!(cm.limit(), (100_000 - 16_384) as f64);
    let read = |id: &str| vec![json!({"id": id, "type": "function", "function": {"name": "read_file", "arguments": "{}"}})];
    cm.add_assistant_message(Some(""), Some(read("a")), None, None);
    cm.add_tool_result("a", &"x".repeat(400_000));
    cm.add_assistant_message(Some(""), Some(read("b")), None, None);
    cm.add_tool_result("b", &"y".repeat(40_000)); // the newest result stays whole; the older one is cleared
    assert_eq!(cm.prune_tool_outputs(), 1);
    assert!(cm.messages[1].content.contains("Re-run the tool") && !cm.messages[1].content.contains("recall_result"), "{}", cm.messages[1].content);
}

#[test]
fn pi_read_cuts_at_2000_lines_and_says_where_to_continue() {
    let body: String = (1..=5000).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
    let out = pi_read(&body, 1, None);
    assert!(out.starts_with("l1\n") && out.contains("l2000\n") && !out.contains("l2001"));
    assert!(out.ends_with("[Showing lines 1-2000 of 5000. Use offset=2001 to continue.]"));
    assert_eq!(pi_read(&body, 4990, None), (4990..=5000).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n"));
    assert!(pi_read("a\nb\nc", 1, Some(2)).ends_with("[1 more lines in file. Use offset=3 to continue.]"));
    assert!(pi_read("a", 9, None).contains("beyond end"));
}

#[test]
fn pi_prompt_is_small_and_lists_only_the_tools_given() {
    let dir = tempfile::tempdir().unwrap();
    let c = cfg(dir.path(), json!({}));
    let tools = create_default_registry(ConfigHandle::new(c.clone())).get_tools();
    let prompt = get_system_prompt(&c, Some("remembered"), &tools, &[]);
    assert!(tools.len() == 11 && prompt.contains("- shell:") && !prompt.contains("- git:") && !prompt.contains("remembered"), "{} tools", tools.len());
}

#[test]
fn pi_cap_keeps_the_tail_of_shell_output_and_the_head_of_everything_else() {
    let dir = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::new(ConfigHandle::new(cfg(dir.path(), json!({}))));
    let text: String = (1..=3000).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
    let (mut sh, mut rd) = (ToolResult::ok(text.clone()), ToolResult::ok(text));
    reg.cap_output(&mut sh, "shell");
    reg.cap_output(&mut rd, "grep");
    assert!(sh.output.contains("line3000") && !sh.output.contains("line1\n") && sh.output.contains("Showing last ") && sh.output.contains("of 3000 lines") && sh.output.len() < 13_000);
    assert!(rd.output.contains("line1\n") && !rd.output.contains("line3000") && rd.output.contains("Showing first 2000 of 3000 lines"));
    let mut small = ToolResult::ok("a\nb".to_string());
    reg.cap_output(&mut small, "shell");
    assert_eq!(small.output, "a\nb");
}

#[test]
fn a_sub_agent_never_gets_the_searcher_and_a_profile_investigator_replaces_the_builtin() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!cfg(dir.path(), json!({"model_role": "review"})).allowed_tools.unwrap().contains(&"subagent_codebase_investigator".to_string()));
    let spec = |tools: &[&str]| json!({"subagents": [{"name": "investigator", "description": "d", "goal_prompt": "g", "allowed_tools": tools}]});
    assert_eq!(cfg(dir.path(), spec(&["read_file", "grep"])).allowed_tools.unwrap().last().unwrap(), "subagent_investigator");
    // a searcher that can run commands is not read-only
    assert!(!cfg(dir.path(), spec(&["read_file", "shell"])).allowed_tools.unwrap().iter().any(|t| t.starts_with("subagent_")));
}

#[tokio::test]
async fn write_file_ends_a_new_file_with_a_newline_but_leaves_a_no_newline_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &'static str, body: &'static str| {
        let inv = ToolInvocation { params: json!({"path": name, "content": body}), cwd: dir.path().to_path_buf(), epoch: 0, call_id: "c".into(), progress: None };
        async move { WriteFileTool.execute(&inv).await }
    };
    write("a.tsx", "export {}").await;
    assert_eq!(std::fs::read_to_string(dir.path().join("a.tsx")).unwrap(), "export {}\n"); // biome/prettier fail a file without it
    std::fs::write(dir.path().join("b.txt"), "old").unwrap(); // this file never had one: keep it that way
    write("b.txt", "new").await;
    assert_eq!(std::fs::read_to_string(dir.path().join("b.txt")).unwrap(), "new");
}

#[test]
fn every_skill_is_listed_with_when_to_use_it() {
    let dir = tempfile::tempdir().unwrap();
    let figma = dir.path().join("elsewhere/figma/SKILL.md");
    let debug = dir.path().join("elsewhere/systematic-debugging/SKILL.md");
    let long = "Build animations in React Native and Expo, making every decision about springs and timing along the way. Use when the user mentions Reanimated or gestures";
    let a = Skill { name: "figma".into(), description: "Work in Figma files".into(), path: Some(figma), ..Default::default() };
    let b = Skill { name: "systematic-debugging".into(), description: long.into(), path: Some(debug), ..Default::default() };
    let rows = skill_rows(&[&a, &b], 110);
    assert!(rows.contains("figma/SKILL.md: Work in Figma files"));
    assert!(rows.contains("systematic-debugging/SKILL.md: Use when the user mentions Reanimated or gestures"), "{rows}"); // the trigger, not the preamble
}

#[tokio::test]
async fn pi_mode_attaches_folder_rules_even_after_a_long_read() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("web")).unwrap();
    std::fs::write(p.join("web/AGENTS.md"), "WEB RULE: ask before restyling").unwrap();
    std::fs::write(p.join("web/big.ts"), (0..5000).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n")).unwrap();
    let c = cfg(p, json!({}));
    let reg = create_default_registry(ConfigHandle::new(c.clone()));
    let res = reg.invoke("read_file", json!({"path": "web/big.ts"}), p, &HookSystem::new(&c), None, "c1").await;
    assert!(res.output.contains("line 1999") && !res.output.contains("line 2001"), "the read itself was cut"); // the read itself was cut
    assert!(res.output.contains("WEB RULE"));
}
