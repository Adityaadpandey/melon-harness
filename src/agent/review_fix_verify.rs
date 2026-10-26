//! A fresh pair of eyes that tries to prove a fix works, the way Claude Code's verification workers do. The author of a
//! fix describes what it meant to do; the diff says what it did. The verifier never saw the author's reasoning, so it
//! cannot inherit its blind spots. It works in a throwaway copy of the change (shell on, OS sandbox on, node_modules linked).
use crate::agent::review_pipeline::ask;
use crate::agent::review_repro::{drop_scratch, make_scratch, REPRO_TOOLS};
use crate::config::Config;
use crate::util::re::lit_i;
use std::path::Path;

const PROMPT: &str = "You are verifying a fix you did not write. Your current directory is a throwaway copy of the changed tree (the fix is applied there as
uncommitted changes on top of HEAD; edit nothing in the real repo, you cannot reach it).

THE FINDINGS THE FIX WAS FOR:
{findings}

WHAT THE AUTHOR SAYS IT DID (a statement of intent, not evidence; the diff is the evidence):
{claimed}

AUTOMATED CHECKS THE HARNESS ALREADY RAN: {checks}

Prove the fix works. Confirming that it exists proves nothing, and a rubber stamp undermines everything.
1. Read the real diff first (`git diff HEAD`). Does it do what the author says? Does it fix the ROOT CAUSE, or only the place the finding named
   (grep every other caller, sibling and mirrored half for the same cause)? Is anything in the diff unrelated to the findings, a new dependency,
   or out of step with how the neighbouring code is written?
2. For each finding, make its trigger happen and show the result: run the repro if one is given, or the test the author added, or write the
   smallest script of your own (a file named .melon-verify.<ext>) that pushes the finding's input through the real code.
3. If the author added a test, show that it fails WITHOUT the fix: `git stash`, run it, `git stash pop` (stash only tracked changes; keep the test file).
4. Run the project's typecheck, lint and the tests that cover the changed files with the change in place. Investigate every error: nothing is
   \"unrelated\" until you have shown it fails the same way on HEAD.
5. Try what the author probably did not: empty and null inputs, the error path, the other caller, old data, the second call in a row.
No network, no installs, no database. A check you cannot run is said so, never assumed to pass.

Reply with exactly these lines and nothing else:
<finding number> | FIXED or NOT_FIXED or PARTIAL | the command you ran and the output line that decides it
NEW | HIGH or MEDIUM or LOW | path:line | a defect or regression the fix itself introduced, or a fix-quality problem (symptom patched, mirror missed, test missing) | the smallest correction
VERDICT: PASS or VERDICT: FAIL
PASS only when every finding is FIXED with evidence you produced and there is no NEW HIGH or MEDIUM line.";

#[derive(Clone, Debug, Default)]
pub struct FixVerdict {
    pub passed: bool,
    /// (number, FIXED|NOT_FIXED|PARTIAL, evidence)
    pub findings: Vec<(usize, String, String)>,
    /// "SEVERITY path:line problem -> fix"
    pub new: Vec<String>,
    /// False when no verifier could run (no scratch copy, no answer): the review alone decides.
    pub ran: bool,
}

impl FixVerdict {
    pub fn feedback(&self) -> String {
        let mut rows: Vec<String> = self.findings.iter().filter(|(_, s, _)| s != "FIXED").map(|(n, s, w)| format!("{n}. {s}: {w}")).collect();
        rows.extend(self.new.iter().map(|x| format!("NEW {x}")));
        if rows.is_empty() { "(no detail)".into() } else { rows.join("\n") }
    }

    pub fn summary(&self) -> String {
        let done = self.findings.iter().filter(|(_, s, _)| s == "FIXED").count();
        format!("{done}/{} finding(s) shown fixed{}", self.findings.len(), if self.new.is_empty() { String::new() } else { format!(", {} new problem(s)", self.new.len()) })
    }
}

pub fn parse(text: &str) -> FixVerdict {
    let (mut findings, mut new) = (vec![], vec![]);
    for line in text.lines() {
        let parts: Vec<String> = line.trim().trim_start_matches(|c| " -*•".contains(c)).split('|').map(|p| p.trim().to_string()).collect();
        if parts.len() >= 3 && parts[0].chars().all(|c| c.is_ascii_digit()) && !parts[0].is_empty() && matches!(parts[1].to_uppercase().as_str(), "FIXED" | "NOT_FIXED" | "PARTIAL") {
            findings.push((parts[0].parse().unwrap_or(0), parts[1].to_uppercase(), parts[2].chars().take(300).collect()));
        } else if parts.len() >= 4 && parts[0].to_uppercase() == "NEW" && matches!(parts[1].to_uppercase().as_str(), "HIGH" | "MEDIUM" | "LOW") {
            let fix = if parts.len() > 4 && !parts[4].is_empty() { format!(" -> {}", parts[4].chars().take(160).collect::<String>()) } else { String::new() };
            new.push(format!("{} {} {}{fix}", parts[1].to_uppercase(), parts[2], parts[3].chars().take(240).collect::<String>()));
        }
    }
    let re = lit_i(r"(?m)^\W*VERDICT:\s*(PASS|FAIL)\b");
    let (mut last, mut at) = (None, 0);
    while at < text.len() {
        let Some(c) = re.captures(&text[at..]) else { break };
        let (_, e) = re.find(&text[at..]).unwrap();
        last = c[1].clone();
        at += e.max(1);
    }
    let Some(v) = last else { return FixVerdict { passed: false, findings, new, ran: false } }; // no verdict line: the verifier did not finish
    let bad = findings.iter().any(|(_, s, _)| s != "FIXED") || new.iter().any(|x| x.starts_with("HIGH") || x.starts_with("MEDIUM"));
    FixVerdict { passed: v.to_uppercase() == "PASS" && !bad, findings, new, ran: true }
}

/// Run the verifier in a scratch copy. Never fails; a verifier that could not run returns ran=false and the review decides alone.
pub async fn verify_fix(config: &Config, role: &str, cwd: &Path, findings: &str, claimed: &str, files: &[String], checks: &str) -> FixVerdict {
    let Some(scratch) = make_scratch(cwd, files, "HEAD", true).await else { return FixVerdict::default() };
    let mut scfg = config.clone();
    scfg.cwd = scratch.clone();
    let prompt = PROMPT.replace("{findings}", findings).replace("{claimed}", &if claimed.is_empty() { "(nothing)".to_string() } else { claimed.chars().take(2500).collect() }).replace("{checks}", if checks.is_empty() { "none ran" } else { checks });
    let reply = ask(&scfg, &prompt, role, 30, config.review_pipeline_tokens, None, Some(REPRO_TOOLS.iter().map(|s| s.to_string()).collect()), "auto").await;
    drop_scratch(cwd, &scratch).await;
    parse(&reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_verdicts() {
        let v = parse("1 | FIXED | ran x: 4 passed\nNEW | LOW | a.ts:2 | meh | fix it\nVERDICT: PASS");
        assert!(v.ran && v.passed);
        assert_eq!(v.summary(), "1/1 finding(s) shown fixed, 1 new problem(s)");
        let v = parse("1 | NOT_FIXED | still throws\nVERDICT: PASS");
        assert!(!v.passed);
        assert!(v.feedback().contains("1. NOT_FIXED: still throws"));
        assert!(!parse("no verdict").ran);
        assert!(!parse("1 | FIXED | x\nNEW | HIGH | a:1 | bad\nVERDICT: PASS").passed);
    }
}
