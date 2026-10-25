//! Ported from tests/test_plan_offer.py
use crate::ui::app::looks_like_plan;

#[test]
fn plan_detection() {
    let steps: Vec<String> = (1..5).map(|i| format!("{i}. Edit file number {i} so that the import path points at the new folder and still works")).collect();
    let plan = format!("Plan to move the pages:\n{}", steps.join("\n"));
    assert!(looks_like_plan(&plan));
    assert!(!looks_like_plan("The handler lives in src/a.ts, line 40."));
    assert!(!looks_like_plan("- one\n- two\n- three")); // too short to be a plan
}
