//! Ported from tests/test_review_removed_behaviour.py
use crate::agent::review_pipeline as rp;

#[test]
fn reviewer_and_triage_look_for_removed_behaviour() {
    assert!(rp::LENSES[0].1.contains("Removed behaviour") && rp::TRIAGE_PROMPT.contains("deleted or moved guard"));
}
