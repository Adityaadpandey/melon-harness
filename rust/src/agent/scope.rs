//! Requests that cover many places ("fix all such issues in the whole app") need a sweep, not a few lucky reads. The
//! sweep makes melon list every place first, then close the list.
use crate::util::re::{lit_i, Re};
use std::sync::OnceLock;

fn broad() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| {
        lit_i(
            r"\b(whole|entire|full)\s+(app|application|codebase|code base|repo|repository|project|site|website|web)\b|\b(all|every|each)\s+(such|the|these|those|other|similar|of the|of these)?\s*(issues?|bugs?|places?|pages?|components?|routes?|files?|buttons?|forms?|screens?|endpoints?|handlers?|usages?|instances?|cases?|occurrences?)\b|\beverywhere\b|\bacross the (app|codebase|repo|project)\b|\bany(?:where)? (similar|such|other)\b",
        )
    })
}

pub fn is_broad(request: &str) -> bool {
    broad().is_match(request)
}

pub const SWEEP: &str = "This request covers many places, so sweep before you fix. (1) Search the area the user named, repo-wide and with several patterns, and list EVERY place that could have the problem (grep for the pattern, `graph importers` for who uses a shared component). (2) Write that list with the todos tool, one step per place or group of places. (3) Work through it: for each step either fix it or note in one line why it is fine. (4) Before you answer, search again and confirm nothing on the list is left. Answer in a few lines: what you changed, and what you ruled out as one short group. Do not stop after the first few files.";

pub const SWEEP_CHECKPOINT: &str = "Checkpoint: update the todos with what you have covered so far (mark done what you fixed or ruled out) and keep going down the list. Do not stop while steps are open.";
