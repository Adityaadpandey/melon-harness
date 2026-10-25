//! Ported from tests/test_repomap.py: the repo map lists the most imported file first and respects its budget.
use super::git;
use crate::util::repomap::{build, build_with};
use std::fs;

#[test]
fn most_imported_file_comes_first() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    fs::write(p.join("core.py"), "class Engine:\n    pass\n\ndef start():\n    pass\n").unwrap();
    for n in ["a", "b", "c"] {
        fs::write(p.join(format!("{n}.py")), format!("import core\n\ndef run_{n}():\n    pass\n")).unwrap();
    }
    git(p, &["init", "-q"]);
    git(p, &["add", "."]);
    let out = build(p).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "core.py: Engine, start");
    assert_eq!(lines.len(), 4);
}

#[test]
fn budget_cuts_the_tail() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    for i in 0..30 {
        fs::write(p.join(format!("m{i}.py")), "def f():\n    pass\n").unwrap();
    }
    git(p, &["init", "-q"]);
    git(p, &["add", "."]);
    assert!(build_with(p, 100).unwrap().len() <= 100);
}
