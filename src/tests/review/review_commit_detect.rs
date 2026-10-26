//! Ported from tests/test_review_commit_detect.py
use crate::agent::verify::{changed_since, Snapshot};

fn snap(rows: &[(&str, &str)]) -> Snapshot {
    rows.iter().map(|(p, h)| (p.to_string(), h.to_string())).collect()
}

#[test]
fn a_commit_is_not_a_change() {
    // committed: the file was dirty before and is clean after, so it must not be listed
    assert!(changed_since(Some(&snap(&[("a.ts", "h1")])), Some(&snap(&[]))).is_empty());
    assert_eq!(changed_since(Some(&snap(&[])), Some(&snap(&[("a.ts", "h1")]))), vec!["a.ts"]);
}
