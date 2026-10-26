//! Ported from tests/test_permission_rules.py.
use crate::safety::rules::{drop_rule, load_rules, parse_rule, save_rule, Rules};
use serde_json::json;
use std::path::Path;

/// Build a rule set from (kind, [rule text]) pairs.
fn rules(dir: &Path, kinds: &[(&str, &[&str])]) -> Rules {
    let rules = kinds.iter().flat_map(|(k, ts)| ts.iter().map(move |t| parse_rule(t, k, "test").unwrap())).collect();
    Rules { rules, cwd: dir.to_path_buf() }
}

fn shell(c: &str) -> serde_json::Value {
    json!({"command": c})
}

#[test]
fn bash_prefix_and_exact() {
    let dir = tempfile::tempdir().unwrap();
    let r = rules(dir.path(), &[("allow", &["Bash(git diff:*)", "Bash(npm run build)"])]);
    assert!(r.decide("shell", &shell("git diff HEAD~1")).is_some());
    assert!(r.decide("shell", &shell("npm run build")).is_some());
    assert!(r.decide("shell", &shell("npm run build --watch")).is_none());
    assert!(r.decide("shell", &shell("git difftool")).is_none()); // a word boundary, not a raw prefix
}

#[test]
fn allow_needs_every_piece_deny_needs_any() {
    let dir = tempfile::tempdir().unwrap();
    assert!(rules(dir.path(), &[("allow", &["Bash(ls:*)"])]).decide("shell", &shell("ls && rm -r x")).is_none());
    assert!(rules(dir.path(), &[("deny", &["Bash(rm:*)"])]).decide("shell", &shell("ls && rm -r x")).is_some());
}

#[test]
fn deny_beats_ask_beats_allow() {
    let dir = tempfile::tempdir().unwrap();
    let r = rules(dir.path(), &[("allow", &["Bash"]), ("ask", &["Bash(git push:*)"]), ("deny", &["Bash(git push --force:*)"])]);
    assert_eq!(r.decide("shell", &shell("git push --force origin")).unwrap().kind, "deny");
    assert_eq!(r.decide("shell", &shell("git push origin")).unwrap().kind, "ask");
    assert_eq!(r.decide("shell", &shell("ls")).unwrap().kind, "allow");
}

#[test]
fn path_patterns() {
    let dir = tempfile::tempdir().unwrap();
    let r = rules(dir.path(), &[("deny", &["Read(.env*)", "Edit(infra/**)"]), ("allow", &["Edit(src/**)"])]);
    let kind = |tool: &str, path: &str| r.decide(tool, &json!({"path": path})).map(|x| x.kind.clone());
    assert_eq!(kind("read_file", ".env.local").as_deref(), Some("deny"));
    assert_eq!(kind("grep", "apps/web/.env").as_deref(), Some("deny")); // a bare name matches at any depth
    assert_eq!(kind("write_file", "infra/main.tf").as_deref(), Some("deny"));
    let nested = dir.path().join("src").join("a").join("b.ts");
    assert_eq!(kind("edit", nested.to_str().unwrap()).as_deref(), Some("allow"));
    assert_eq!(kind("edit", "docs/readme.md"), None);
}

#[test]
fn webfetch_domain() {
    let dir = tempfile::tempdir().unwrap();
    let r = rules(dir.path(), &[("allow", &["WebFetch(domain:python.org)"])]);
    assert!(r.decide("web_fetch", &json!({"url": "https://docs.python.org/3/"})).is_some());
    assert!(r.decide("web_fetch", &json!({"url": "https://evilpython.org/"})).is_none());
}

#[test]
fn files_roundtrip_and_claude_settings() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    save_rule(d, "deny", "Read(.env*)").unwrap();
    std::fs::create_dir_all(d.join(".claude")).unwrap();
    std::fs::write(d.join(".claude/settings.json"), r#"{"permissions": {"allow": ["Bash(make:*)"]}}"#).unwrap();
    let r = load_rules(d);
    assert_eq!(r.decide("shell", &shell("make test")).unwrap().kind, "allow");
    assert_eq!(r.decide("read_file", &json!({"path": ".env"})).unwrap().kind, "deny");
    assert!(drop_rule(d, "Read(.env*)"));
    assert!(load_rules(d).decide("read_file", &json!({"path": ".env"})).is_none());
}
