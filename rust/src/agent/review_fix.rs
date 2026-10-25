//! Fixing what a review found, the way a senior engineer does it instead of the way a quick patch does. After a review
//! the user says "fix them", "fix 1 and 3" or "review and fix". The findings of that review are still in memory, so the
//! fix run starts from the facts the reviewers and the verifier already established.
use crate::agent::review_pipeline::Finding;
use crate::tools::builtin::advisor::AdvisorTool;
use crate::tools::registry::{Gate, ToolRegistry};
use crate::util::re::{lit_i, Re};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

fn negated() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(don'?t|do not|no need to|without|never)\s+(\w+\s+)?fix"))
}
fn fix_word() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\bfix(ing|es|ed)?\b"))
}
fn fix_reply() -> &'static Re {
    // "fix them", "yes fix it", "go ahead and fix these", "fix all", "fix the issues/findings/bugs", "fix 1 and 3", "fix #2"
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"^\W*(?:(?:yes|yep|yeah|ok(?:ay)?|sure|please|go ahead(?: and)?|do it|and)\b[\s,.!]*)*fix\b[\s\w#,&-]*?(?:\b(?:them|it|these|those|all|everything|the (?:issues?|findings?|bugs?|problems?|items?))\b|#?\d)"))
}
fn numbers() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"#?\b(\d{1,2})\b"))
}

/// A reply that is mostly "fix <the findings>": short, starts with fix (or yes/ok/go ahead and fix).
pub fn asks_for_fix(message: &str) -> bool {
    message.chars().count() < 200 && !negated().is_match(message) && fix_reply().is_match(message)
}

/// One message that asks for both ("review this PR and fix what you find").
pub fn review_and_fix(message: &str) -> bool {
    fix_word().is_match(message) && !negated().is_match(message)
}

/// The findings the user pointed at: "fix 1 and 3" is items 1 and 3 of the list the answer showed; anything else is
/// every proven one. Guesses (unverified, unclear) are only fixed when named by number.
pub fn chosen(message: &str, shown: &[Finding]) -> Vec<Finding> {
    let mut nums: Vec<usize> = vec![];
    let mut at = 0;
    while let Some(c) = numbers().captures(&message[at..]) {
        let (_, e) = numbers().find(&message[at..]).unwrap();
        if let Some(n) = c[1].as_ref().and_then(|x| x.parse::<usize>().ok()) {
            if n > 0 && n <= shown.len() && !nums.contains(&n) {
                nums.push(n);
            }
        }
        at += e.max(1);
    }
    if !nums.is_empty() {
        return nums.into_iter().map(|n| shown[n - 1].clone()).collect();
    }
    shown.iter().filter(|f| f.verdict == "CONFIRMED").cloned().collect()
}

const RULES: &str = "HOW TO FIX THESE (work like the senior engineer who owns this code):
1. Read before you change. For each finding open the cited lines, then the callers, the consumers and the other half of any pair (desktop/mobile,
   create/update, server/client, cached/uncached). Fix the ROOT CAUSE once, where every path goes through it, not a guard in the one caller the
   finding names. If the suggested fix treats a symptom, say so and fix the cause.
2. Call the `advisor` tool BEFORE your first edit (after you have read the code): it sees this whole conversation and checks your diagnosis
   and plan. Weigh its advice seriously; if the code you read contradicts it, call advisor again with \"I found X, you said Y, which wins?\".
   The edit tools refuse to run until you have consulted it.
3. Smallest diff that is fully correct: match how the neighbouring code does the same thing (naming, comments, error handling), no drive-by
   cleanup, no new dependency, no reformatting of lines you do not need to touch. Deleting code beats adding a fallback.
4. Prove each fix. Where a repro is given, run it before and after. Otherwise add a regression test in the repo's own style that fails without
   your change and passes with it (look at the neighbouring tests for the runner and helpers). Then run the typecheck, lint and the tests that
   cover the files you touched. A failure is never \"unrelated\" until you have read it and shown why.
5. Before you say you are done: re-read your own diff (`git diff`) as a reviewer would, check every other place the same cause appears, then call
   `advisor` once more with the finished work. Make the work durable first (files saved).
6. Do not commit, push or touch files unrelated to the findings.

DONE MEANS: for every finding, the trigger in its description no longer produces the bad result, a test or repro shows it, the checks pass, and
nothing outside the finding changed behaviour.

FINAL ANSWER, one block per finding: `file:line` of the change, why this is the root cause (one sentence), how you verified it (the command and
what it printed). Findings you chose not to fix, or could not, get the reason. No other prose.";

/// The task a fix run is handed. Everything the review learned travels with it.
pub fn brief(findings: &[Finding]) -> String {
    let rows: Vec<String> = findings
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mut row = vec![format!("{}. [{}] {} {}", i + 1, f.verdict.to_lowercase(), f.severity, f.where_), format!("   problem: {}", f.what)];
            if !f.evidence.is_empty() {
                row.push(format!("   what the verifier read: {}", f.evidence));
            }
            if !f.reach.is_empty() && f.reach != "n/a" {
                row.push(format!("   affects: {}", f.reach));
            }
            if !f.fix.is_empty() {
                row.push(format!("   suggested fix: {}", f.fix));
            }
            if !f.repro.is_empty() {
                row.push(format!("   repro: {}", f.repro));
            }
            row.join("\n")
        })
        .collect();
    format!("FINDINGS TO FIX (from the review you just gave; each was checked by a separate agent):\n\n{}\n\n{RULES}", rows.join("\n\n"))
}

/// The same findings as one line each, for the reviewer that later judges the fix.
pub fn short(findings: &[Finding]) -> String {
    findings.iter().enumerate().map(|(i, f)| format!("{}. {} {}: {}", i + 1, f.severity, f.where_, f.what)).collect::<Vec<_>>().join("\n")
}

pub const ADVISOR_FIRST: &str = "Consult the advisor before you edit anything: call the `advisor` tool now (no arguments). It reads this whole conversation and checks your diagnosis and plan.";
pub const ADVISOR_LAST: &str = "You changed code after your last advisor consultation (or never consulted it). Call the `advisor` tool now with the finished work, act on what it says (fix it, or say why the code you read contradicts it), then give the final answer.";

/// The registry refuses edit/write_file while the advisor has not been consulted this request.
pub fn edit_gate(registry: Arc<ToolRegistry>) -> Gate {
    Arc::new(move |name: &str| {
        let calls = registry.get("advisor").and_then(|t| t.as_any().downcast_ref::<AdvisorTool>().map(|a| a.calls.load(Ordering::SeqCst))).unwrap_or(1);
        (matches!(name, "edit" | "write_file") && calls == 0).then(|| ADVISOR_FIRST.to_string())
    })
}

/// True when this request changed files (`base` is the registry's write count when it started) after the advisor was
/// last consulted, or without ever consulting it.
pub fn needs_final_advice(registry: &ToolRegistry, base: u64) -> bool {
    let writes = registry.writes.load(Ordering::SeqCst);
    let last = registry.get("advisor").and_then(|t| t.as_any().downcast_ref::<AdvisorTool>().map(|a| a.last_call_at.load(Ordering::SeqCst))).unwrap_or(-1);
    writes > base && writes as i64 > last
}

#[allow(dead_code)]
fn _unused(_: HashSet<u8>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: &str) -> Finding {
        Finding { severity: "HIGH".into(), where_: "a.ts:1".into(), what: "w".into(), fix: "x".into(), verdict: v.into(), ..Default::default() }
    }

    #[test]
    fn replies() {
        assert!(asks_for_fix("fix them"));
        assert!(asks_for_fix("yes, fix 1 and 3"));
        assert!(asks_for_fix("go ahead and fix all"));
        assert!(!asks_for_fix("don't fix anything"));
        assert!(!asks_for_fix("please add a feature"));
        assert!(review_and_fix("review this PR and fix what you find"));
        assert!(!review_and_fix("review it, don't fix"));
    }

    #[test]
    fn picks() {
        let shown = vec![f("CONFIRMED"), f("PLAUSIBLE"), f("CONFIRMED")];
        assert_eq!(chosen("fix 2", &shown).len(), 1);
        assert_eq!(chosen("fix them", &shown).len(), 2);
        assert!(brief(&shown).contains("1. [confirmed] HIGH a.ts:1"));
    }
}
