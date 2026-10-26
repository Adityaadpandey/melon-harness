//! Ported from tests/test_guard_profile.py.
use super::trust::isolated_home;
use crate::agent::verify::select_checks;
use crate::config::loader::{instructions_for, load_config, LoadOptions};
use crate::config::{ApprovalPolicy, Config, VerifyRule};
use crate::safety::approval::{ApprovalContext, ApprovalDecision, ApprovalManager};
use crate::safety::guard::violation;
use crate::tests::fakes::*;
use serde_json::json;
use std::path::Path;

fn cfg(dir: &Path) -> Config {
    let mut c = config(dir);
    c.forbidden_paths = vec![".env".into(), ".env.*".into(), "*.pem".into()];
    c.forbidden_commands = vec![r"\bpsql\b".into(), r"prisma\s+(studio|migrate|db)\b".into()];
    c
}

fn shell(c: &str) -> serde_json::Value {
    json!({"command": c})
}

fn path(p: &str) -> serde_json::Value {
    json!({"path": p})
}

#[test]
fn protected_files_blocked_for_every_tool_shape() {
    let dir = tempfile::tempdir().unwrap();
    let c = cfg(dir.path());
    assert!(violation("read_file", &path(".env"), &c).is_some());
    assert!(violation("read_file", &path("apps/server/.env.production"), &c).is_some());
    assert!(violation("read_file", &path("certs/server.pem"), &c).is_some());
    assert!(violation("shell", &shell("cat .env.local"), &c).is_some());
    assert!(violation("shell", &shell("grep KEY ./apps/.env"), &c).is_some());
    assert!(violation("shell", &shell("source<.env"), &c).is_some());
    assert!(violation("read_file", &path("src/environment.ts"), &c).is_none());
}

#[test]
fn database_commands_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let c = cfg(dir.path());
    for cmd in ["psql -h prod", "npx prisma migrate dev", "pnpm prisma studio", "PSQL x"] {
        assert!(violation("shell", &shell(cmd), &c).is_some(), "{cmd}");
    }
    assert!(violation("shell", &shell("npx prisma generate"), &c).is_none());
    assert!(violation("shell", &shell("npx tsc --noEmit"), &c).is_none());
}

#[test]
fn no_rules_means_no_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(dir.path());
    c.forbidden_paths = vec![];
    c.forbidden_commands = vec![];
    assert!(violation("shell", &shell("cat .env"), &c).is_none());
}

#[tokio::test]
async fn guard_applies_through_registry() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "API_KEY=sekret").unwrap();
    let mut c = cfg(dir.path());
    c.approval = ApprovalPolicy::Auto;
    c.auto_verify = false;
    let mut a = scripted(c, vec![call("read_file", json!({"path": ".env"})), text("ok")]).await;
    let evs = run(&mut a, "read it").await;
    let done = evs.iter().find_map(|e| match e {
        crate::agent::events::AgentEvent::ToolCallComplete { result, .. } => Some(result.clone()),
        _ => None,
    });
    let done = done.expect("the read_file call completes");
    assert!(!done.success);
    assert!(!format!("{done:?}").contains("sekret"));
}

#[test]
fn verify_rules_select_by_changed_path_and_expand_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = cfg(dir.path());
    c.verify_rules = vec![
        VerifyRule { paths: vec!["apps/server/".into()], cwd: "apps/server".into(), commands: vec!["tsc".into(), "biome {files}".into()], tests_glob: None },
        VerifyRule { paths: vec!["apps/web/".into()], cwd: "apps/web".into(), commands: vec!["web-check".into()], tests_glob: None },
    ];
    let got = select_checks(&c, &["apps/server/src/a b.ts".to_string(), "README.md".to_string()]);
    assert_eq!(got, vec![("tsc".to_string(), "apps/server".to_string()), ("biome 'src/a b.ts'".to_string(), "apps/server".to_string())]);
    assert!(select_checks(&c, &["docs/x.md".to_string()]).is_empty());
}

/// A repo folder (with .git, so the search stops there) and an empty home, so only the files the test writes count.
fn repo(dir: &Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    dir.join("nohome")
}

#[test]
fn large_instruction_files_become_an_outline() {
    let dir = tempfile::tempdir().unwrap();
    let nohome = repo(dir.path());
    std::fs::write(dir.path().join("AGENTS.md"), format!("# Rules\n{}## Server\nmore\n", "filler line\n".repeat(8000))).unwrap();
    let out = instructions_for(dir.path(), Some(&nohome)).unwrap();
    assert!(out.contains("line 1: # Rules") && out.contains("## Server") && !out.contains("filler"));
}

#[test]
fn small_instruction_files_inline_and_stubs_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let nohome = repo(dir.path());
    std::fs::write(dir.path().join("AGENTS.md"), "Always run the linter before finishing. ".repeat(3)).unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "@AGENTS.md").unwrap();
    let out = instructions_for(dir.path(), Some(&nohome)).unwrap();
    assert!(out.contains("Always run the linter") && !out.contains("@AGENTS.md"));
}

#[test]
fn tunyt_profile_is_selected_by_path_and_project_config_wins() {
    isolated_home();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tunyt").join("apps").join("server");
    std::fs::create_dir_all(root.join(".melon")).unwrap();
    std::fs::write(root.join(".melon").join("config.toml"), "max_turns = 7\n").unwrap();
    let opts = || LoadOptions { trust_project: false, pi_mode: None };
    let c = load_config(Some(&root), &opts()).unwrap();
    assert_eq!(c.profile.as_deref(), Some("tunyt"));
    assert!(c.forbidden_paths.iter().any(|p| p == ".env"));
    assert_eq!(c.max_turns, 7);
    assert!(c.developer_instructions.unwrap().contains("Tunyt profile"));
    let other = dir.path().join("somewhere");
    std::fs::create_dir_all(&other).unwrap();
    assert!(load_config(Some(&other), &opts()).unwrap().profile.is_none());
}

#[test]
fn symlinked_cwd_does_not_make_inside_edits_look_outside() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let m = ApprovalManager::new(ApprovalPolicy::Auto, &link, &[]);
    let ctx = ApprovalContext {
        tool_name: "write_file".into(),
        is_mutating: true,
        affected_paths: vec![real.join("a.py")],
        command: None,
        is_dangerous: false,
        tool_kind: "write".into(),
    };
    assert_eq!(m.check_approval(&ctx), ApprovalDecision::Approved);
}

#[test]
fn user_global_and_sibling_instruction_files_are_all_read() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join("AGENTS.md"), "Run the linter before finishing any change here. ".repeat(2)).unwrap();
    std::fs::write(repo.join("CLAUDE.md"), "Commit messages are one short imperative line. ".repeat(2)).unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude").join("CLAUDE.md"), "Never put a sign-off in commit messages. ".repeat(2)).unwrap();
    let out = instructions_for(&repo, Some(&home)).unwrap();
    assert!(out.contains("Run the linter") && out.contains("one short imperative") && out.contains("sign-off"));
}

#[test]
fn long_rulebook_keeps_commit_rules_and_follows_imports() {
    let dir = tempfile::tempdir().unwrap();
    let nohome = repo(dir.path());
    std::fs::write(dir.path().join("RULES.md"), "Commit messages never carry a sign-off line, ever.\n").unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), format!("# Rules\n@RULES.md\n{}Always ask before writing a spec.\n", "filler line\n".repeat(8000))).unwrap();
    let out = instructions_for(dir.path(), Some(&nohome)).unwrap();
    assert!(out.contains("too long to inline") && out.contains("never carry a sign-off") && out.contains("ask before writing a spec"));
}

#[test]
fn a_big_agents_md_like_tunyts_goes_in_whole() {
    let dir = tempfile::tempdir().unwrap();
    let nohome = repo(dir.path());
    // about 48k chars
    std::fs::write(dir.path().join("AGENTS.md"), format!("# Rules\n{}LAST RULE\n", "Never run SQL against prod.\n".repeat(1700))).unwrap();
    let out = instructions_for(dir.path(), Some(&nohome)).unwrap();
    assert!(!out.contains("too long to inline") && out.contains("LAST RULE"));
}
