//! Ported from tests/test_verify_js.py: which cheap check the harness picks for the files a run changed.
use crate::agent::verify::detect_checks;
use std::path::Path;

fn touch(p: &Path, rel: &str, text: &str) {
    let f = p.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(f, text).unwrap();
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

#[test]
fn only_the_changed_files_are_checked_never_a_build_or_the_whole_suite() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    touch(p, "pnpm-lock.yaml", "");
    touch(p, "package.json", r#"{"scripts": {"build": "turbo build", "typecheck": "tsc --noEmit"}}"#);
    touch(p, "apps/web/package.json", r#"{"scripts": {"check-types": "tsc --noEmit"}}"#);
    touch(p, "node_modules/.bin/eslint", "x");
    touch(p, "apps/web/src/a.tsx", "x");
    touch(p, "pkg/store.py", "x");
    touch(p, "tests/test_store.py", "x");
    touch(p, "pkg/util.py", "x");
    assert_eq!(
        detect_checks(p, &s(&["apps/web/src/a.tsx", "pkg/store.py", "README.md"])),
        vec![pair("python3 -m pytest -q -x tests/test_store.py", "."), pair("pnpm exec eslint --quiet src/a.tsx", "apps/web")]
    );
    // no test for it: a syntax check
    assert_eq!(detect_checks(p, &s(&["pkg/util.py"])), vec![pair("python3 -m py_compile pkg/util.py", ".")]);
}

#[test]
fn nothing_for_files_with_no_cheap_check() {
    // a Swift app's Tests/ folder is tests/ on macOS: it once made melon run pytest on Swift and call "no tests ran" a failure
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    touch(p, "Tests/check.swift", "x");
    touch(p, "Sources/main.swift", "x");
    touch(p, "web/a.ts", "x"); // eslint not installed
    assert!(detect_checks(p, &s(&["Sources/main.swift", "web/a.ts", "gone.py"])).is_empty());
}

#[test]
fn yaml_config_gets_parsed_so_a_broken_stack_file_is_not_called_done() {
    // the deploy files an agent hand-edits (alert rules, compose) used to get no check at all
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    touch(p, "monitoring/rules.yml", "a: 1\n");
    touch(p, "README.md", "x");
    let checks = detect_checks(p, &s(&["monitoring/rules.yml", "README.md"]));
    if std::env::var_os("PATH").map_or(false, |v| std::env::split_paths(&v).any(|d| d.join("python3").is_file())) {
        assert_eq!(checks.len(), 1);
        assert!(checks[0].0.starts_with("python3 -c ") && checks[0].0.ends_with("monitoring/rules.yml"));
    }
}
