//! Ported from tests/test_review_triage.py
use crate::agent::review_pipeline as rp;

#[test]
fn triage_questions_become_reviewers() {
    let reply = "CHECK | a.ts:10 | does the new null return break callers?\nnoise\nCHECK | b.ts:3 | is the lock still released on error?";
    let checks = rp::parse_triage(reply);
    assert!(checks[0].starts_with("a.ts:10: ") && checks.len() == 2);
    assert!(rp::parse_triage("NONE").is_empty());
    assert_eq!(rp::triage_lenses(&checks).len(), 1); // two questions: one reviewer
    let six: Vec<String> = checks.iter().cycle().take(6).cloned().collect();
    assert_eq!(rp::triage_lenses(&six).len(), 2); // six: two
}
