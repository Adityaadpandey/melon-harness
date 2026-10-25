//! Ported from tests/test_root.py.
use crate::agent::root::pick_root;
use std::path::{Path, PathBuf};

fn repo(p: PathBuf) -> PathBuf {
    std::fs::create_dir_all(&p).unwrap();
    assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(&p).status().unwrap().success());
    p
}

#[test]
fn picks_the_repo_only_when_something_says_which() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let (a, b) = (repo(base.join("tunyt")), repo(base.join("other")));
    assert_eq!(pick_root(base, "fix the share link", None, &[]), None); // two repos, no signal
    assert_eq!(pick_root(base, "fix the share link in Tunyt", None, &[]), Some(a.clone())); // named
    let touched = vec![format!("{}/src/x.ts:foo", b.display())];
    assert_eq!(pick_root(base, "fix it", None, &touched), Some(b)); // files already touched
    assert_eq!(pick_root(base, "fix it", Some(a.as_path()), &[]), Some(a.clone())); // what the last request used
    assert_eq!(pick_root(&a, "fix it in other", None, &[]), None); // already inside a repo: leave cwd alone
}

#[test]
fn a_single_repo_below_is_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let a = repo(dir.path().join("only"));
    assert_eq!(pick_root(Path::new(dir.path()), "anything", None, &[]), Some(a));
}
