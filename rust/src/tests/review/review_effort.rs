//! Ported from tests/test_review_effort.py
use crate::agent::review_pipeline as rp;

fn files(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn small_change_is_light_and_a_big_or_risky_one_is_not() {
    assert!(rp::is_light(&"+x\n".repeat(16), &files(&["deploy.yml"]), false));
    assert!(!rp::is_light(&"+x\n".repeat(200), &files(&["a.ts"]), false));
    assert!(!rp::is_light(&"+x\n".repeat(5), &files(&["a.ts", "b.ts", "c.ts"]), false));
    assert!(!rp::is_light(&"+x\n".repeat(5), &files(&["auth.ts"]), true));
}

#[test]
fn quick_pass_note_is_attached_and_asks_for_few_reads() {
    assert!(rp::LIGHT_NOTE.contains("QUICK PASS") && rp::LIGHT_NOTE.contains("at most 3 reads"));
}
