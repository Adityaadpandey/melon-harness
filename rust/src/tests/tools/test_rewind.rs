//! Ported from tests/test_rewind.py: rewinding two requests restores the first one's starting point, deleting files
//! either request created.
use super::git;
use crate::agent::verify::{backup, undo};
use std::fs;

#[tokio::test]
async fn rewind_across_two_requests() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q"]);
    git(p, &["config", "user.email", "t@t"]);
    git(p, &["config", "user.name", "t"]);
    fs::write(p.join("a.txt"), "v0").unwrap();
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "init"]);
    let ref1 = backup(p).await.unwrap();
    fs::write(p.join("a.txt"), "v1").unwrap();
    fs::write(p.join("new1.txt"), "x").unwrap();
    backup(p).await; // request 2 starts here
    fs::write(p.join("a.txt"), "v2").unwrap();
    fs::write(p.join("new2.txt"), "y").unwrap();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let (_restored, mut deleted, skipped) = undo(p, &ref1, &s(&["a.txt", "new1.txt", "new2.txt"]), &s(&["new1.txt", "new2.txt"])).await;
    assert_eq!(fs::read_to_string(p.join("a.txt")).unwrap(), "v0");
    assert!(!p.join("new1.txt").exists() && !p.join("new2.txt").exists());
    deleted.sort();
    assert_eq!(deleted, vec!["new1.txt", "new2.txt"]);
    assert!(skipped.is_empty());
}
