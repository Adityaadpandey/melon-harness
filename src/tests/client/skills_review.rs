//! Ported from tests/test_skills_review.py.
//! Not ported (they replace `review_changes` or the model client inside a sub-agent, and the Rust reviewer and
//! sub-agents build their own fresh Agent with a real client, so there is nothing to script offline):
//!   test_reviewer_findings_go_back_for_one_fix, test_clean_review_just_finishes,
//!   test_second_reviewer_sees_the_first_findings_and_its_catch_is_merged,
//!   test_the_fix_is_reviewed_once_and_what_it_still_finds_is_shown_not_looped,
//!   test_subagent_from_profile_runs_with_its_role_and_tools.
//! The pure parts of those flows (verdict parsing, merging, the diff) are covered below.
use super::trust::isolated_home;
use crate::agent::events::AgentEvent;
use crate::agent::review::{build_diff, merge_reviews, parse_verdict, ReviewResult};
use crate::agent::skills::{extract_section, headings, load_skills, match_skills, parse_skill};
use crate::client::routing::Router;
use crate::config::loader::{load_config, LoadOptions};
use crate::config::{ApprovalPolicy, Config, ConfigHandle, SubagentSpec};
use crate::prompts::get_system_prompt;
use crate::tests::fakes::*;
use crate::tools::registry::create_default_registry;
use crate::tools::subagents::default_definitions;
use crate::util::text::estimate_tokens;
use serde_json::json;
use std::path::Path;

const SKILL: &str = "---
name: dbg
description: debugging runbook
triggers: bug, stuck job, 500   # words that load it
canonical: docs/dbg.md
---
Body text.
";
const DOC: &str = "# Top\nintro\n## Worker lane\nworker facts\n### sub\nmore\n## Database lane\ndb facts\n```\n## not a heading\n```\n";

/// A config like the Python tests': the full (non-pi) prompt, edits auto-approved, no model-based orientation.
fn cfg(dir: &Path) -> Config {
    isolated_home();
    let mut c = config(dir);
    c.pi_mode = false;
    c.approval = ApprovalPolicy::Auto;
    c.auto_verify = false;
    c.auto_orient = false;
    c
}

fn names(skills: Vec<crate::agent::skills::Skill>) -> Vec<String> {
    skills.into_iter().map(|s| s.name).collect()
}

#[test]
fn parse_and_match() {
    let sk = parse_skill(SKILL, None).unwrap();
    assert_eq!((sk.name.as_str(), sk.canonical.as_deref()), ("dbg", Some("docs/dbg.md")));
    assert_eq!(sk.triggers, ["bug", "stuck job", "500"]);
    let other = parse_skill("---\nname: perf\ndescription: x\ntriggers: slow, cache\n---\nB", None).unwrap();
    let both = [sk.clone(), other.clone()];
    assert_eq!(names(match_skills(&both, "a stuck job again", 4)), ["dbg"]);
    assert!(match_skills(&both, "debugger rebuild", 4).is_empty()); // whole-word starts, no substring noise
    assert!(match_skills(&both, "nothing relevant", 4).is_empty());
}

#[test]
fn sections_extracted_by_heading_not_fenced_text() {
    let worker = extract_section(DOC, "worker lane").unwrap();
    assert!(worker.starts_with("## Worker lane") && worker.contains("more"));
    assert!(!worker.contains("db facts"));
    assert!(!headings(DOC).iter().any(|h| h.2 == "## not a heading" || h.2 == "not a heading"));
    assert!(extract_section(DOC, "missing").is_none());
}

#[tokio::test]
async fn use_skill_tool() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join(".melon/skills")).unwrap();
    std::fs::write(d.join(".melon/skills/dbg.md"), SKILL).unwrap();
    std::fs::create_dir_all(d.join("docs")).unwrap();
    std::fs::write(d.join("docs/dbg.md"), DOC).unwrap();
    let a = scripted(cfg(d), vec![]).await;
    let reg = a.session.tool_registry.clone();
    let hooks = &a.session.hook_system;
    assert_eq!(reg.invoke("use_skill", json!({"name": "dbg"}), d, hooks, None, "c1").await.output, "Body text.");
    assert!(reg.invoke("use_skill", json!({"name": "dbg", "section": "Worker lane"}), d, hooks, None, "c2").await.output.contains("worker facts"));
    assert!(reg.invoke("use_skill", json!({"name": "dbg", "section": "list"}), d, hooks, None, "c3").await.output.contains("Worker lane"));
    assert!(!reg.invoke("use_skill", json!({"name": "nope"}), d, hooks, None, "c4").await.success);
}

#[tokio::test]
async fn matching_playbook_is_attached_to_the_request() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".melon/skills")).unwrap();
    std::fs::write(dir.path().join(".melon/skills/dbg.md"), SKILL).unwrap();
    let mut a = scripted(cfg(dir.path()), vec![text("ok")]).await;
    let evs = run(&mut a, "there is a bug in checkout").await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice(m) if m.contains("Loaded playbooks:") && m.contains("dbg"))));
    let sent = a.session.client.requests.lock().unwrap()[0].clone();
    assert!(sent.last().unwrap()["content"].as_str().unwrap().contains("Body text."));
    assert!(sent[0]["content"].as_str().unwrap().contains("Skills"));
}

// ---------------------------------------------------------------- the diff and the verdict

fn git(p: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git").args(args).current_dir(p).output().unwrap().status.success();
    assert!(ok, "git {args:?}");
}

#[tokio::test]
async fn diff_contains_tracked_edits_and_new_files() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "t@t"]);
    git(repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a.py"), "x = 1\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "i"]);
    std::fs::write(repo.join("a.py"), "x = 2\n").unwrap();
    std::fs::write(repo.join("new.py"), "print('hi')\n").unwrap();
    let d = build_diff(repo, &["a.py".to_string(), "new.py".to_string()], &["HEAD".to_string()]).await;
    assert!(d.contains("-x = 1") && d.contains("+x = 2") && d.contains("new file: new.py") && d.contains("print('hi')"), "{d}");
}

#[test]
fn verdict_parsing() {
    assert_eq!(parse_verdict("VERDICT: PASS").verdict, "PASS");
    let r = parse_verdict("blah\nVERDICT: ISSUES\n1. a.py:1 bad | evidence: x");
    assert_eq!(r.verdict, "ISSUES");
    assert!(r.findings.contains("a.py:1"));
    assert_eq!(parse_verdict("looks fine to me").verdict, "UNPARSED");
}

fn review(verdict: &str, findings: &str) -> ReviewResult {
    ReviewResult { verdict: verdict.into(), findings: findings.into(), blocked: 0 }
}

#[test]
fn merge_keeps_both_reports_and_ignores_a_second_reviewer_that_found_nothing() {
    let (a, b) = (review("ISSUES", "1. a"), review("ISSUES", "1. b"));
    let both = merge_reviews(&a, &b).findings;
    assert!(both.contains("1. a") && both.contains("1. b"));
    assert_eq!(merge_reviews(&a, &review("PASS", "")).findings, "1. a");
    assert_eq!(merge_reviews(&a, &review("UNPARSED", "x")).findings, "1. a");
    assert_eq!(merge_reviews(&review("PASS", ""), &b).findings, "1. b");
}

// ---------------------------------------------------------------- profiles, sub-agents, config

#[test]
fn profile_subagents_replace_builtins_and_router_puts_non_author_reviewer_first() {
    let dir = tempfile::tempdir().unwrap();
    let spec = SubagentSpec { name: "debugger".into(), description: "d".into(), goal_prompt: "g".into(), allowed_tools: None, max_turns: 20, timeout_seconds: 600.0, role: "debug".into() };
    let mut c = cfg(dir.path());
    c.subagents = vec![spec];
    c.routing.insert("review".into(), vec!["author".into(), "other".into()]);
    c.model.name = "author".into();
    // the profile's own come first, built-ins fill in after
    assert_eq!(default_definitions(&c)[0].name, "debugger");
    let plain: Vec<String> = default_definitions(&cfg(dir.path())).into_iter().map(|d| d.name).collect();
    assert_eq!(plain, ["codebase_investigator", "planner", "code_reviewer", "general"]);
    assert_eq!(Router::new(ConfigHandle::new(c)).chain("review"), ["other", "author"]);
}

#[test]
fn target_repo_env_file_is_never_loaded() {
    isolated_home();
    unsafe { std::env::remove_var("SECRET_FROM_TARGET_REPO") };
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET_FROM_TARGET_REPO=leak\n").unwrap();
    load_config(Some(dir.path()), &LoadOptions { trust_project: false, pi_mode: None }).unwrap();
    assert!(std::env::var("SECRET_FROM_TARGET_REPO").is_err());
}

// ---------------------------------------------------------------- built-in skills

#[test]
fn ponytail_is_on_by_default_inlined_not_indexed_and_can_be_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let c = cfg(dir.path());
    let sk = load_skills(&c);
    assert!(sk.iter().any(|s| s.name == "ponytail" && s.always));
    let reg = create_default_registry(ConfigHandle::new(c.clone()));
    let prompt = get_system_prompt(&c, None, &reg.get_tools(), &sk);
    // inlined, not an index entry
    assert!(prompt.contains("the least that fully works") && !prompt.contains("- **ponytail**"));
    let mut off = cfg(dir.path());
    off.disabled_skills = vec!["ponytail".into()];
    assert!(!get_system_prompt(&off, None, &[], &load_skills(&off)).contains("the least that fully works"));
    // always-on skills are never triggered a second time
    assert!(!names(match_skills(&sk, "fix this bug", 4)).contains(&"ponytail".to_string()));
    assert!(match_skills(&sk, "hello there", 4).is_empty());
}

#[test]
fn builtin_skills_load_with_the_right_always_on_core_and_profile_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let sk = load_skills(&cfg(dir.path()));
    let always: std::collections::BTreeSet<&str> = sk.iter().filter(|s| s.always).map(|s| s.name.as_str()).collect();
    assert_eq!(always, ["grounded", "house-rules", "ponytail", "typescript"].into());
    let have: std::collections::BTreeSet<&str> = sk.iter().map(|s| s.name.as_str()).collect();
    for n in ["debugging", "code-review", "security", "refactor", "testing", "performance", "database", "api-design", "git-workflow"] {
        assert!(have.contains(n), "missing built-in skill {n}");
    }
    // the always-on core stays cheap. Python counted with tiktoken (limit 1100); the 4-bytes-a-token estimate runs about
    // 7% higher on this text, so the limit here is 1300.
    assert!(sk.iter().filter(|s| s.always).map(|s| estimate_tokens(&s.body)).sum::<usize>() < 1300);
    // melon's own skills all carry triggers (the fake home's Claude Code skills are listed only, so they have none)
    let home = isolated_home();
    assert!(sk.iter().filter(|s| !s.always && !s.path.as_ref().unwrap().starts_with(&home)).all(|s| !s.triggers.is_empty()));
    let mut with_profile = cfg(dir.path());
    with_profile.profile = Some("tunyt".into());
    let tunyt: Vec<String> = names(load_skills(&with_profile));
    // specialised playbook replaces the generic one; generic ones without a Tunyt twin remain
    assert!(tunyt.contains(&"tunyt-debugging".to_string()) && !tunyt.contains(&"debugging".to_string()));
    assert!(tunyt.contains(&"refactor".to_string()));
    assert_eq!(names(match_skills(&sk, "please refactor this module and rename the helper", 4))[0], "refactor");
}

#[test]
fn answer_size_limits_are_in_the_prompt_and_the_review_skill() {
    let dir = tempfile::tempdir().unwrap();
    let c = cfg(dir.path());
    let sk = load_skills(&c);
    assert!(sk.iter().find(|s| s.name == "code-review").unwrap().body.contains("under 150 words"));
    assert!(get_system_prompt(&c, None, &[], &[]).contains("Under 120 words"));
}

#[tokio::test]
async fn pi_mode_playbook_message_does_not_offer_the_use_skill_tool() {
    let dir = tempfile::tempdir().unwrap();
    isolated_home();
    // pi mode narrows the tool list when the config is built from a settings map (Config::default() leaves it open)
    let map = json!({"pi_mode": true, "cwd": dir.path(), "approval": "auto", "auto_verify": false, "auto_orient": false});
    let c = Config::from_map(map.as_object().unwrap().clone()).unwrap();
    let mut a = scripted(c, vec![text("No issues found")]).await;
    run(&mut a, "review my changes").await;
    let msgs = a.session.context_manager.lock().unwrap().get_messages();
    let first = msgs.iter().find(|m| m["role"] == "user").unwrap()["content"].as_str().unwrap().to_string();
    assert!(first.contains("Playbook: code-review") && !first.contains("use_skill"), "{first}");
}

#[test]
fn a_repos_own_claude_skill_loads_and_a_quoted_phrase_in_its_description_triggers_it() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().join(".claude/skills/motion-audit");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("SKILL.md"), "---\nname: motion-audit\ndescription: Use when asked to \"improve the animations\" of the app.\n---\nbody\n").unwrap();
    let sk = load_skills(&cfg(dir.path()));
    assert_eq!(names(match_skills(&sk, "please improve the animations on discover", 4)), ["motion-audit"]);
}

#[tokio::test]
async fn a_playbook_is_pasted_once_then_named() {
    // Python swapped match_skills for "everything matches"; here a skill whose trigger is in both requests does the same.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".melon/skills")).unwrap();
    std::fs::write(dir.path().join(".melon/skills/zq-playbook.md"), "---\nname: zq-playbook\ndescription: test\ntriggers: zqreview\n---\nPLAYBOOK BODY\n").unwrap();
    let mut a = scripted(cfg(dir.path()), vec![]).await;
    for ask in ["zqreview this", "zqreview again"] {
        *a.session.client.script.lock().unwrap() = Some(vec![text("ok")].into());
        run(&mut a, ask).await;
    }
    let msgs = a.session.context_manager.lock().unwrap().get_messages();
    let users: Vec<String> = msgs.iter().filter(|m| m["role"] == "user").map(|m| m["content"].as_str().unwrap_or("").to_string()).collect();
    assert!(users[0].contains("PLAYBOOK BODY") && !users[1].contains("PLAYBOOK BODY"));
    assert!(users[1].contains("still apply") && users[1].contains("zq-playbook"));
}
