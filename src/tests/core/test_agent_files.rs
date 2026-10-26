//! Ported from tests/test_agent_files.py: Claude Code style agent files become sub-agent definitions.
use crate::tools::subagents::{default_definitions, parse_agent_file, READONLY_TOOLS};

const FILE: &str = "---
name: db-reviewer
description: Reviews SQL and migrations. Use after schema changes.
tools: Read, Grep, Bash
model: haiku
---
You review migrations for locks and data loss.
";

#[test]
fn claude_code_agent_file_parses() {
    let d = parse_agent_file(FILE).unwrap();
    assert_eq!(d.name, "db_reviewer");
    assert_eq!(d.allowed_tools.unwrap(), vec!["read_file", "grep", "shell"]);
    assert_eq!(d.role, "fast");
    assert!(d.goal_prompt.contains("locks"));
}

#[test]
fn file_without_name_or_body_is_skipped() {
    assert!(parse_agent_file("---\ndescription: x\n---\nbody").is_none());
    assert!(parse_agent_file("---\nname: a\n---\n").is_none());
}

#[test]
fn repo_agents_join_the_built_ins() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".claude/agents")).unwrap();
    std::fs::write(tmp.path().join(".claude/agents/db.md"), FILE).unwrap();
    let names: Vec<String> = default_definitions(&super::cfg(tmp.path(), serde_json::json!({}))).into_iter().map(|d| d.name).collect();
    for want in ["codebase_investigator", "planner", "code_reviewer", "general", "db_reviewer"] {
        assert!(names.iter().any(|n| n == want), "{want} missing from {names:?}");
    }
}

#[test]
fn planner_and_investigator_cannot_edit() {
    let tmp = tempfile::tempdir().unwrap();
    for d in default_definitions(&super::cfg(tmp.path(), serde_json::json!({}))) {
        if d.name == "planner" || d.name == "codebase_investigator" {
            for t in d.allowed_tools.unwrap() {
                assert!(READONLY_TOOLS.contains(&t.as_str()), "{} may use {t}", d.name);
            }
        }
    }
}
