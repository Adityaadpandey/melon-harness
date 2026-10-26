//! Ported from tests/test_precompact.py: tool output is tidied before it reaches the model.
use crate::util::precompact::pre_compact;

#[test]
fn ansi_progress_and_repeats_are_dropped_but_the_error_stays() {
    let bars: String = (1..100).map(|i| format!("Downloading  {i}%|{}", "#".repeat(i / 5))).collect::<Vec<_>>().join("\n");
    let lint: String = (0..40).map(|i| format!("src/a.ts:{i}:1 error no-unused-vars 'x{i}' is defined")).collect::<Vec<_>>().join("\n");
    let out = pre_compact(&format!("\x1b[31mFAILED\x1b[0m\n{bars}\n{lint}\nAssertionError: boom"));
    assert!(!out.contains('\x1b') && out.contains("FAILED") && out.contains("AssertionError: boom"));
    assert_eq!(out.matches("Downloading").count(), 1);
    assert_eq!(out.matches("no-unused-vars").count(), 3);
    assert!((out.len() as f64) < 0.2 * (bars.len() + lint.len()) as f64);
}

#[test]
fn long_python_traceback_keeps_both_ends() {
    let frames: String = (0..30).map(|i| format!("  File \"m{i}.py\", line {i}, in f{i}\n    call{i}()\n")).collect();
    let out = pre_compact(&format!("Traceback (most recent call last):\n{frames}ValueError: bad {}", "x".repeat(400)));
    assert!(out.contains("File \"m0.py\"") && out.contains("File \"m29.py\"") && !out.contains("File \"m15.py\"") && out.contains("ValueError: bad"));
}

#[test]
fn short_text_is_untouched() {
    assert_eq!(pre_compact("ok\n\x1b[1mdone"), "ok\n\x1b[1mdone");
}
