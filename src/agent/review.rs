//! Independent review pass. A separate agent (its own context, read-only tools, and, when routing says so, a different
//! model) reads the REAL diff and the original request and hunts for concrete defects. The author never gets to grade
//! its own work.
use crate::agent::skills::{load_skills, match_skills};
use crate::config::{ApprovalPolicy, Config};
use crate::util::re::lit_i;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DIFF_CAP: usize = 40_000;
const NEW_FILE_CAP: usize = 6_000;

const REVIEW_PROMPT: &str = "You are an independent code reviewer. You did not write this change.

ORIGINAL REQUEST:
{request}

FILES CHANGED: {files}

AUTOMATED CHECKS: {checks}

THE AUTHOR'S EXPLANATION OF THE CHANGE:
{claims}

FILES THAT DEPEND ON THE CHANGED CODE (from the code graph; read their call sites before claiming a break):
{impact}

{context}{prior}{fixing}DIFF:
{diff}

First test the author's explanation: for each factual claim about why the old code failed (\"X collapses\", \"Y is never called\"), read the code and say whether it is true. A fix built on a claim the code contradicts, such as handling something existing code already handles, is a defect. Then review for: requirements the request asked for but the diff misses; logic errors; broken callers or contracts
(read the callers with your tools before claiming a break); security, authorization and tenant-scoping problems;
concurrency/idempotency problems; missing handling of edge cases; violations of the project rules you were given.
Only report problems you can point to in the code. No style nitpicks, no speculation, no generic praise.

Reply in EXACTLY this format:
VERDICT: PASS
or
VERDICT: ISSUES
1. <file:line> <what is wrong and why it matters> | evidence: <what you read that proves it>
2. ...
";

/// The reviewer prompts already carry their method.
const COVERED_BY_LENSES: &[&str] = &["code-review", "review-changes", "review-pr", "review-commits"];

/// What a human reviewer would have open next to the diff: the AGENTS.md / CLAUDE.md of every folder the change touches
/// (root first, @imports inlined) and the playbooks this request calls for (typescript, security, ...).
pub fn review_context(config: &Config, request: &str, files: &[String]) -> String {
    let root = PathBuf::from(&config.cwd);
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut rules = vec![];
    for f in files.iter().take(40) {
        let text = crate::agent::instructions::for_path(&root, &root.join(f), &mut seen);
        if !text.is_empty() {
            rules.push(text.split_once("\n\n").map_or(text.clone(), |x| x.1.to_string())); // drop the "shown once" banner, keep the files
        }
    }
    let mut out = vec![];
    if !rules.is_empty() {
        out.push(format!("PROJECT RULES (from the repo's instruction files; a change that breaks one is a finding, quote the rule):\n{}", rules.join("\n\n").chars().take(6000).collect::<String>()));
    }
    let skills = load_skills(config);
    let picked: Vec<_> = match_skills(&skills, &format!("{request} {}", files.iter().take(30).cloned().collect::<Vec<_>>().join(" ")), 8).into_iter().filter(|s| !COVERED_BY_LENSES.contains(&s.name.as_str())).take(3).collect();
    if !picked.is_empty() {
        out.push(format!("PLAYBOOKS THAT APPLY (the project's own checklists):\n{}", picked.iter().map(|s| format!("### {}\n{}", s.name, s.body)).collect::<Vec<_>>().join("\n\n").chars().take(5000).collect::<String>()));
    }
    let loaded: HashSet<&String> = picked.iter().map(|s| &s.name).collect();
    let others: Vec<_> = skills.iter().filter(|s| !loaded.contains(&s.name) && !COVERED_BY_LENSES.contains(&s.name.as_str()) && !s.always && !s.description.is_empty()).collect();
    if !others.is_empty() {
        // Claude Code lists every skill and lets the model open the ones that fit; so does the reviewer (use_skill)
        out.push(format!(
            "MORE SKILLS (yours, the repo's and Claude Code's). If one fits this change, open it with use_skill BEFORE you review and check the diff against it:\n{}",
            others.iter().take(30).map(|s| format!("- {}: {}", s.name, s.description.chars().take(110).collect::<String>())).collect::<Vec<_>>().join("\n")
        ));
    }
    out.join("\n\n")
}

#[derive(Clone, Debug)]
pub struct ReviewResult {
    /// PASS | ISSUES | UNPARSED
    pub verdict: String,
    pub findings: String,
    /// Reviewer tool calls a hook or rule refused: its checks were narrower than intended.
    pub blocked: usize,
}

pub async fn git(cwd: &Path, args: &[&str]) -> String {
    tokio::process::Command::new("git").args(args).current_dir(cwd).stderr(std::process::Stdio::null()).output().await.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

/// Tracked files via git diff against `rev`; untracked (new) files shown whole, since git diff omits them. A committed
/// range (two revs, e.g. HEAD~3 HEAD) is just that range: the working tree is not part of it.
pub async fn build_diff(cwd: &Path, files: &[String], rev: &[String]) -> String {
    let working = rev.len() == 1;
    let tracked: HashSet<String> = if files.is_empty() {
        HashSet::new()
    } else {
        let mut a = vec!["ls-files", "--"];
        a.extend(files.iter().map(String::as_str));
        git(cwd, &a).await.split('\n').map(String::from).collect()
    };
    let mut parts = vec![];
    let gone: HashSet<&String> = files.iter().filter(|f| !cwd.join(f).exists()).collect(); // deleted or moved away: git diff shows what was removed
    let mut pick: Vec<&String> = if working { files.iter().filter(|f| tracked.contains(*f) || gone.contains(f)).collect() } else { files.iter().collect() };
    pick.sort();
    pick.dedup();
    if !pick.is_empty() {
        let mut a: Vec<&str> = vec!["diff"];
        a.extend(rev.iter().map(String::as_str));
        a.push("--");
        a.extend(pick.iter().map(|s| s.as_str()));
        parts.push(git(cwd, &a).await);
    }
    if working {
        for f in files {
            if !tracked.contains(f) && cwd.join(f).is_file() {
                let body = String::from_utf8_lossy(&std::fs::read(cwd.join(f)).unwrap_or_default()).to_string();
                let clip = if body.chars().count() > NEW_FILE_CAP { "\n... [clipped]" } else { "" };
                parts.push(format!("--- new file: {f}\n{}{clip}", body.chars().take(NEW_FILE_CAP).collect::<String>()));
            }
        }
    }
    let diff = parts.join("\n");
    if diff.chars().count() > DIFF_CAP {
        format!("{}\n... [diff clipped]", diff.chars().take(DIFF_CAP).collect::<String>())
    } else {
        diff
    }
}

pub fn parse_verdict(text: &str) -> ReviewResult {
    let re = lit_i(r"(?s)VERDICT:\s*(PASS|ISSUES)\b(.*)");
    // last one wins: a model may restate the format before answering
    let mut last = None;
    let mut at = 0;
    while at < text.len() {
        let Some(c) = re.captures(&text[at..]) else { break };
        let (s, _) = re.find(&text[at..]).unwrap();
        last = Some((c[1].clone().unwrap_or_default(), c[2].clone().unwrap_or_default()));
        at += s + 1;
        while at < text.len() && !text.is_char_boundary(at) {
            at += 1;
        }
    }
    match last {
        None => ReviewResult { verdict: "UNPARSED".into(), findings: text.chars().take(500).collect(), blocked: 0 },
        Some((v, rest)) => ReviewResult { verdict: v.to_uppercase(), findings: rest.trim().to_string(), blocked: 0 },
    }
}

const SECOND_LOOK: &str = "A first reviewer already reported this (it may be wrong or incomplete):
{prior}

Do not repeat it. Hunt for defects it MISSED, and say so if one of its findings is false.

";

const FIX_REVIEW: &str = "THIS IS A REVIEW OF A FIX. The author was told about these problems and changed the code to address them:
{fixing}

Judge the fix, not the original idea: is each problem really resolved? Then look for what the fix itself broke: other branches of the same function (a changed return shape), every consumer of a changed output (types, tests, API collections), loading/error/empty states that the new condition no longer covers, and whether a restriction (e.g. hiding data) can still be bypassed another way (e.g. searching by the hidden field).

";

#[allow(clippy::too_many_arguments)]
pub async fn review_changes(config: &Config, request: &str, files: &[String], checks: &[Value], claims: &str, fixing: Option<&str>, prior: Option<&str>, role: &str) -> ReviewResult {
    let diff = build_diff(&config.cwd, files, &["HEAD".to_string()]).await;
    let mut cfg = config.clone();
    cfg.model_role = role.into();
    cfg.allowed_tools = Some(["read_file", "grep", "search", "outline", "git", "glob", "list_dir", "use_skill"].iter().map(|s| s.to_string()).collect());
    cfg.auto_review = false;
    cfg.auto_verify = false;
    cfg.verify_commands = vec![];
    cfg.verify_rules = vec![];
    cfg.subagents = vec![];
    cfg.max_turns = if diff.matches('\n').count() < 80 { 8 } else { 14 }; // small diffs get 8 turns: a look, not an expedition
    cfg.max_seconds = 300.0;
    cfg.approval = ApprovalPolicy::Never;
    let mut impact = "(no code graph)".to_string();
    let live = crate::util::symgraph::load(&config.cwd).impact(files, 15);
    if !live.is_empty() {
        impact = live.iter().map(|(f, l)| format!("- {f} ({})", l.join(", "))).collect::<Vec<_>>().join("\n");
    } else if let Some(g) = crate::util::codegraph::load(&config.cwd) {
        let rows = g.impact(files, 15);
        impact = if rows.is_empty() { "(none found)".into() } else { rows.iter().map(|(f, l)| format!("- {f} ({})", l.join(", "))).collect::<Vec<_>>().join("\n") };
    }
    let summary = if checks.is_empty() { "none ran".to_string() } else { checks.iter().map(|c| format!("{}={}", c["command"].as_str().unwrap_or(""), if c["ok"] == true { "ok" } else { "FAILED" })).collect::<Vec<_>>().join(", ") };
    let mut context = review_context(config, request, files);
    if !context.is_empty() {
        context.push_str("\n\n");
    }
    let clip = |s: &str, n: usize| -> String { s.chars().take(n).collect() };
    let prompt = REVIEW_PROMPT
        .replace("{request}", &clip(request, 3000))
        .replace("{files}", &files.iter().take(30).cloned().collect::<Vec<_>>().join(", "))
        .replace("{checks}", &summary)
        .replace("{claims}", &if claims.is_empty() { "(none given)".to_string() } else { clip(claims, 2500) })
        .replace("{impact}", &impact)
        .replace("{context}", &context)
        .replace("{prior}", &prior.filter(|p| !p.is_empty()).map(|p| SECOND_LOOK.replace("{prior}", &clip(p, 3000))).unwrap_or_default())
        .replace("{fixing}", &fixing.filter(|p| !p.is_empty()).map(|p| FIX_REVIEW.replace("{fixing}", &clip(p, 3000))).unwrap_or_default())
        .replace("{diff}", &diff);
    let c = crate::agent::agent::run_collect(cfg, &prompt).await;
    let mut result = parse_verdict(&c.texts.join("\n")); // the verdict may sit in an earlier turn than the last text
    result.blocked = c.blocked;
    if result.verdict == "UNPARSED" && !c.error.is_empty() {
        result.findings = format!("reviewer stopped: {}", clip(&c.error, 300));
    } else if result.verdict == "UNPARSED" && c.texts.is_empty() {
        result.findings = "reviewer produced no answer".into();
    }
    result
}

/// Either reviewer finding something makes it ISSUES; a second reviewer that crashed changes nothing.
pub fn merge_reviews(first: &ReviewResult, second: &ReviewResult) -> ReviewResult {
    if second.verdict != "ISSUES" {
        return first.clone();
    }
    if first.verdict != "ISSUES" {
        return second.clone();
    }
    ReviewResult { verdict: "ISSUES".into(), findings: format!("{}\n\nSecond reviewer, additionally:\n{}", first.findings, second.findings), blocked: first.blocked + second.blocked }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verdicts() {
        let r = parse_verdict("VERDICT: PASS or ISSUES\nthinking\nVERDICT: ISSUES\n1. a.py:3 bad | evidence: x");
        assert_eq!(r.verdict, "ISSUES");
        assert!(r.findings.starts_with("1. a.py:3"));
        assert_eq!(parse_verdict("nothing").verdict, "UNPARSED");
        let a = ReviewResult { verdict: "ISSUES".into(), findings: "x".into(), blocked: 0 };
        let b = ReviewResult { verdict: "PASS".into(), findings: "".into(), blocked: 0 };
        assert_eq!(merge_reviews(&b, &a).verdict, "ISSUES");
        assert_eq!(merge_reviews(&a, &b).findings, "x");
    }
}
