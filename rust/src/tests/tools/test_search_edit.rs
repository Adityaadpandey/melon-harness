//! Ported from tests/test_search_edit.py: grep, glob, edit planning, the edit tool, search ranking, outline,
//! the read-only git tool, atomic writes and path checks on sub-agent reports.
use super::{git, handle, invocation, run_tool};
use crate::tools::builtin::{edit_file::EditTool, git::GitTool, glob::GlobTool, grep::GrepTool, outline::OutlineTool, read_file::ReadFileTool, search::SearchTool};
use crate::tools::Tool;
use crate::util::edits::plan_edits;
use crate::util::outline::outline;
use crate::util::paths::atomic_write;
use crate::util::search::check_paths;
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

fn w(p: &Path, rel: &str, text: &str) {
    let f = p.join(rel);
    fs::create_dir_all(f.parent().unwrap()).unwrap();
    fs::write(f, text).unwrap();
}

/// A small git repo with an ignored folder, a log, a secret and node_modules, all containing `load_user`.
fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q"]);
    w(p, ".gitignore", "ignored/\n*.log\n");
    w(p, "src/a.py", "def load_user():\n    return 1\n\nclass Cache: pass\n");
    w(p, "src/b.ts", "export const loadUser = 1;\n");
    w(p, ".github/ci.yml", "run: load_user\n");
    w(p, "ignored/x.py", "load_user secret\n");
    w(p, "debug.log", "load_user\n");
    w(p, ".env", "TOKEN=load_user\n");
    w(p, "node_modules/m.js", "load_user\n");
    d
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[tokio::test]
async fn grep_respects_gitignore_finds_dotdirs_and_skips_secrets() {
    let d = repo();
    let t = GrepTool::new(handle(d.path()));
    let r = run_tool(&t, d.path(), json!({"pattern": "load_user", "files_only": true})).await;
    let files: HashSet<&str> = r.output.lines().map(|l| l.split(' ').next().unwrap()).collect();
    // not ignored/, *.log, .env, node_modules
    assert_eq!(files, HashSet::from(["src/a.py", ".github/ci.yml"]));
}

#[tokio::test]
async fn grep_smart_case_context_glob_and_limit() {
    let d = repo();
    let p = d.path();
    // made up front: another test holds the global file listing for a moment, and a file added after the
    // first search of this repo could then be missed
    w(p, "many.txt", &"hit\n".repeat(30));
    let t = GrepTool::new(handle(p));
    // lowercase + alternation: smart case is on
    assert!(run_tool(&t, p, json!({"pattern": "loaduser|loadUser"})).await.output.contains("src/b.ts:1:"));
    // has an uppercase letter: case-sensitive
    assert!(run_tool(&t, p, json!({"pattern": "Cache"})).await.output.contains("src/a.py:4:class Cache"));
    let r = run_tool(&t, p, json!({"pattern": "return", "context": 1})).await;
    assert!(r.output.contains("src/a.py-1-def load_user") && r.output.contains("src/a.py:2:    return 1"));
    let r = run_tool(&t, p, json!({"pattern": "load", "glob": "*.ts"})).await;
    assert!(r.output.contains("b.ts") && !r.output.contains("a.py"));
    let r = run_tool(&t, p, json!({"pattern": "hit", "limit": 5})).await;
    assert_eq!(r.output.matches("many.txt:").count(), 5);
    assert!(r.output.contains("25 more matches"));
}

#[tokio::test]
async fn grep_too_broad_lists_files_and_missing_path_suggests_real_one() {
    let d = repo();
    let p = d.path();
    for i in 0..20 {
        w(p, &format!("f{i}.txt"), &"hit\n".repeat(6));
    }
    let t = GrepTool::new(handle(p));
    let r = run_tool(&t, p, json!({"pattern": "hit", "limit": 10})).await;
    assert!(r.output.contains("too broad") && r.output.contains("f3.txt") && !r.output.contains("hit\n"));
    let r = run_tool(&t, p, json!({"pattern": "x", "path": "nope/a.py"})).await;
    assert!(!r.success && r.error.unwrap().contains("src/a.py"));
}

#[tokio::test]
async fn grep_anchors_match_per_line_in_crlf_files_too() {
    // `TODO$` on a Windows "TODO\r\n" line must agree with the per-line search, not just the whole-file shortcut
    let d = repo();
    let p = d.path();
    fs::write(p.join("win.txt"), b"first\r\nfix it TODO\r\nlast\r\n").unwrap();
    w(p, "unix.txt", "first\nfix it TODO\n");
    let t = GrepTool::new(handle(p));
    let r = run_tool(&t, p, json!({"pattern": "TODO$"})).await;
    assert!(r.output.contains("win.txt:2:fix it TODO") && r.output.contains("unix.txt:2:fix it TODO"), "{}", r.output);
    assert!(run_tool(&t, p, json!({"pattern": "^last"})).await.output.contains("win.txt:3:last"));
    // \A means line start, as it always did
    assert!(run_tool(&t, p, json!({"pattern": r"\Afix"})).await.output.contains("unix.txt:2:fix"));
}

#[tokio::test]
async fn grep_literal_and_bad_regex() {
    let d = repo();
    let t = GrepTool::new(handle(d.path()));
    assert!(run_tool(&t, d.path(), json!({"pattern": "(", "literal": true})).await.success);
    let r = run_tool(&t, d.path(), json!({"pattern": "("})).await;
    assert!(!r.success && r.error.unwrap().contains("literal=true"));
}

#[tokio::test]
async fn glob_uses_gitignore_and_path_patterns() {
    let d = repo();
    let p = d.path();
    let t = GlobTool::new(handle(p));
    assert_eq!(run_tool(&t, p, json!({"pattern": "*.py"})).await.output, "src/a.py");
    assert_eq!(run_tool(&t, p, json!({"pattern": "src/**/*.ts"})).await.output, "src/b.ts");
    assert!(run_tool(&t, p, json!({"pattern": ".github/*.yml"})).await.output.contains(".github/ci.yml"));
}

#[test]
fn plan_exact_multi_and_overlap() {
    let src = "a = 1\nb = 2\nc = 3\n";
    let (new, n) = plan_edits(src, &pairs(&[("a = 1", "a = 10"), ("c = 3", "c = 30")]), false).unwrap();
    assert_eq!((new.as_str(), n), ("a = 10\nb = 2\nc = 30\n", 2));
    let err = plan_edits(src, &pairs(&[("a = 1\nb = 2", "x"), ("b = 2\nc = 3", "y")]), false).unwrap_err();
    assert!(err.contains("overlap"), "{err}");
}

#[test]
fn plan_reports_all_locations_when_not_unique() {
    let err = plan_edits("x\ny\nx\n", &pairs(&[("x", "z")]), false).unwrap_err();
    assert!(err.contains("2 times (lines 1, 3)"), "{err}");
    let (new, n) = plan_edits("x\ny\nx\n", &pairs(&[("x", "z")]), true).unwrap();
    assert_eq!((new.as_str(), n), ("z\ny\nz\n", 2));
}

#[test]
fn plan_fuzzy_trailing_space_and_smart_quotes() {
    let src = "say(\"hello\")   \nother\n";
    let (new, _) = plan_edits(src, &pairs(&[("say(\u{201c}hello\u{201d})\n", "say(\"bye\")\n")]), false).unwrap();
    assert_eq!(new, "say(\"bye\")\nother\n");
}

#[test]
fn plan_preserves_crlf() {
    let (new, _) = plan_edits("a\r\nb\r\n", &pairs(&[("a", "A")]), false).unwrap();
    assert_eq!(new, "A\r\nb\r\n");
}

#[test]
fn plan_not_found_gives_closest_lines() {
    let err = plan_edits("def load_user(id):\n    pass\n", &pairs(&[("def load_user(uid):", "x")]), false).unwrap_err();
    assert!(err.contains("not found") && err.contains("def load_user(id)"), "{err}");
}

#[tokio::test]
async fn edit_tool_multi_edit_crlf_and_no_approval_for_failed_edit() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    let f = p.join("f.txt");
    fs::write(&f, b"one\r\ntwo\r\nthree\r\n").unwrap();
    let t = EditTool;
    let inv = invocation(p, json!({"path": "f.txt", "edits": [{"old_string": "one", "new_string": "1"}, {"old_string": "three", "new_string": "3"}]}));
    let conf = t.get_confirmation(&inv).await.unwrap();
    assert!(conf.diff.unwrap().to_diff().contains("-one"));
    assert!(t.execute(&inv).await.success);
    assert_eq!(fs::read(&f).unwrap(), b"1\r\ntwo\r\n3\r\n");
    let bad = invocation(p, json!({"path": "f.txt", "old_string": "nope", "new_string": "x"}));
    assert!(t.get_confirmation(&bad).await.is_none()); // nothing to approve
    assert!(!t.execute(&bad).await.success);
}

#[tokio::test]
async fn edit_creates_new_file_with_empty_old_string() {
    let d = tempfile::tempdir().unwrap();
    let r = run_tool(&EditTool, d.path(), json!({"path": "sub/new.py", "new_string": "x = 1\n"})).await;
    assert!(r.success);
    assert_eq!(fs::read_to_string(d.path().join("sub/new.py")).unwrap(), "x = 1\n");
}

#[tokio::test]
async fn search_ranks_files_by_plain_words() {
    let d = repo();
    let p = d.path();
    w(p, "src/EventAbout.tsx", "// blank lines vanish on render\nexport const EventAbout = 1;\n");
    w(p, "src/unrelated.py", "def other():\n    return 'cache'\n");
    w(p, "docs/notes.md", &"event about blank lines render\n".repeat(5));
    let r = run_tool(&SearchTool::new(handle(p)), p, json!({"query": "event about blank lines"})).await;
    let files: Vec<&str> = r.output.lines().filter(|l| !l.starts_with(' ')).map(|l| l.split_whitespace().next().unwrap_or("")).collect();
    assert_eq!(files[0], "src/EventAbout.tsx", "{}", r.output);
    let pos = |n: &str| files.iter().position(|f| *f == n).unwrap_or(usize::MAX);
    assert!(pos("src/EventAbout.tsx") < pos("docs/notes.md"));
}

#[tokio::test]
async fn outline_and_read_symbol_return_one_function() {
    let d = repo();
    let p = d.path();
    w(p, "big.ts", "export function alpha() {\n  return 1;\n}\n\nexport const beta = async (x: number) => {\n  return x;\n};\n\nclass Box {\n  size() {\n    return 2;\n  }\n}\n");
    w(p, "m.py", "class A:\n    def f(self):\n        return 1\n\ndef g():\n    return 2\n");
    let out = run_tool(&OutlineTool, p, json!({"path": "big.ts"})).await.output;
    assert!(out.contains("1-3") && out.contains("function alpha") && out.contains("const beta") && out.contains("method size"), "{out}");
    assert!(run_tool(&OutlineTool, p, json!({"path": "m.py"})).await.output.contains("def f"));
    let rf = ReadFileTool::new(handle(p));
    let r = run_tool(&rf, p, json!({"path": "big.ts", "symbol": "beta"})).await;
    assert!(r.output.contains("return x;") && !r.output.contains("alpha"));
    let r = run_tool(&rf, p, json!({"path": "big.ts", "symbol": "nope"})).await;
    assert!(!r.success && r.error.unwrap().contains("outline"));
}

#[tokio::test]
async fn git_tool_is_read_only_and_finds_changes() {
    let d = repo();
    let p = d.path();
    git(p, &["add", "-A"]);
    git(p, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "add loader"]);
    let t = GitTool;
    assert!(run_tool(&t, p, json!({"op": "log", "path": "src/a.py"})).await.output.contains("add loader"));
    assert!(run_tool(&t, p, json!({"op": "find_change", "query": "load_user"})).await.output.contains("add loader"));
    assert!(run_tool(&t, p, json!({"op": "blame", "path": "src/a.py", "lines": "1,1"})).await.output.contains("load_user"));
    assert!(!run_tool(&t, p, json!({"op": "log", "rev": "--output=x"})).await.success);
    // "commit" is not an allowed op, so validation (which runs before execute) rejects it
    assert!(t.validate_params(&json!({"op": "commit"})).is_err());
}

#[test]
fn outline_maps_jsx_sections_inside_a_big_component() {
    let src = "export default function Page() {\n  return (\n    <div>\n      {/* About the Event */}\n      {x && (\n        <A />\n      )}\n\n      {/* Footer */}\n      <B />\n    </div>\n  );\n}\n";
    let names: std::collections::HashMap<String, (usize, usize)> = outline("Page.tsx", src).into_iter().map(|s| (s.name, (s.start, s.end))).collect();
    assert_eq!(names["Page"], (1, 13));
    assert_eq!(names["About the Event"], (4, 7));
    assert_eq!(names["Footer"].0, 9);
}

#[cfg(unix)]
#[test]
fn atomic_write_keeps_mode_crlf_and_leaves_no_temp_files() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("x.sh");
    fs::write(&f, b"old\r\n").unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    atomic_write(&f, "new\r\nline\r\n").unwrap();
    assert_eq!(fs::read(&f).unwrap(), b"new\r\nline\r\n");
    assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o755);
    let names: Vec<String> = fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert_eq!(names, vec!["x.sh"]);
}

#[tokio::test]
async fn git_scope_reports_branch_uncommitted_and_branch_commits() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q", "-b", "main"]);
    git(p, &["config", "user.email", "a@b"]);
    git(p, &["config", "user.name", "t"]);
    w(p, "a.txt", "1\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "base"]);
    git(p, &["checkout", "-qb", "feature"]);
    w(p, "b.txt", "2\n");
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "add b"]);
    w(p, "a.txt", "changed\n");
    let out = run_tool(&GitTool, p, json!({"op": "scope"})).await.output;
    for want in ["branch: feature", "add b", "a.txt", "b.txt"] {
        assert!(out.contains(want), "{want} missing in {out}");
    }
}

fn two_file_repo(names: [&str; 2]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q"]);
    git(p, &["config", "user.email", "t@t"]);
    git(p, &["config", "user.name", "t"]);
    for n in names {
        w(p, n, "one\n");
    }
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "init"]);
    d
}

#[tokio::test]
async fn git_patch_returns_every_changed_file_in_one_call() {
    let d = two_file_repo(["a.txt", "b.txt"]);
    let p = d.path();
    w(p, "a.txt", "one\ntwo\n");
    w(p, "b.txt", "changed\n");
    let r = run_tool(&GitTool, p, json!({"op": "patch"})).await;
    assert!(r.success && r.output.contains("+two") && r.output.contains("+changed"), "{}", r.output);
}

#[tokio::test]
async fn git_patch_keeps_paths_with_spaces_as_separate_files() {
    let d = two_file_repo(["sp ace.txt", "b.txt"]);
    let p = d.path();
    w(p, "sp ace.txt", "two\n");
    w(p, "b.txt", "changed\n");
    let r = run_tool(&GitTool, p, json!({"op": "patch"})).await;
    assert_eq!(r.output.matches("diff --git").count(), 2, "{}", r.output);
}

#[test]
fn a_report_s_missing_paths_are_flagged_with_the_real_file_of_that_name() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    w(p, "apps/server/src/services/event/event-read.service.ts", "x");
    w(p, "apps/web/src/app/(detail)/[id]/Page.tsx", "x");
    let report = "Start at apps/web/src/app/(detail)/[id]/Page.tsx:12, then apps/server/src/services/event-read.service.ts and apps/web/src/info/Rows.tsx. Docs: https://example.com/a/b.js";
    let notes = check_paths(p, report, &[]);
    assert!(notes.contains("apps/server/src/services/event-read.service.ts: not on disk; files with that name: apps/server/src/services/event/event-read.service.ts"), "{notes}");
    assert!(notes.contains("apps/web/src/info/Rows.tsx: not on disk; locate it"));
    assert!(!notes.contains("Page.tsx") && !notes.contains("example.com"));
    assert_eq!(check_paths(p, "all good: apps/web/src/app/(detail)/[id]/Page.tsx", &[]), "");
}

#[test]
fn edit_copied_with_read_file_line_numbers_still_applies() {
    let src = "def a():\n    return 1\n\n\ndef b():\n    return 2\n";
    let (new, _) = plan_edits(src, &pairs(&[("5|def b():\n6|    return 2", "5|def b():\n6|    return 3")]), false).unwrap();
    assert!(new.ends_with("def b():\n    return 3\n"), "{new}");
    // code that merely starts with digits is not mistaken for a prefix
    assert!(plan_edits("x = 1\n", &pairs(&[("1|x = 2", "y")]), false).is_err());
}

#[test]
fn byte_order_mark_does_not_block_a_first_line_edit() {
    let (new, _) = plan_edits("\u{feff}import os  \nprint(1)\n", &pairs(&[("import os\nprint(1)", "import sys\nprint(1)")]), false).unwrap();
    assert!(new.starts_with("\u{feff}import sys"), "{new}"); // matched, and the mark is kept
}
