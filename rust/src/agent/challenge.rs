//! The challenger: a second model whose only job is to argue against the plan. Before melon acts on a risky request
//! (a design choice, a refactor, a migration), a read-only agent on a different model reads the request and the code
//! and answers: what could this cost, what is being assumed, what is unverified. Its objections are cited or dropped.
use crate::agent::grounding::{extract_refs, verify_references};
use crate::config::{ApprovalPolicy, Config};
use crate::util::re::{lit_i, Re};
use std::path::Path;
use std::sync::OnceLock;

/// A different model family from the author; override with [routing] challenge = [...].
const CHALLENGER: &str = "deepseek/deepseek-v4-pro";

fn risky() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| {
        lit_i(r"\b(should (we|i)|instead of|rather than|approach|design|architect\w*|migrat\w+|refactor|rewrite|redesign|replace|remove|delete|drop|deprecate|rename|split|merge|switch to|move (to|from)|i (think|want|plan|suggest|recommend)|we (should|could|will)|recommend\w*|best way|is it (ok|safe|fine)|strategy|schema|breaking)\b")
    })
}

const PROMPT: &str = "You are the challenger. Your job is to argue against the plan below, not to help carry it out.

REQUEST (including any approach or recommendation the user gave):
{request}

Find what the user may be getting wrong. Look for: what could be LOST (data, behaviour, callers, compatibility, time);
what is ASSUMED but not checked; a simpler or safer way; a requirement that is missing. Use your tools to read the
real code before you object. Do not object to style. Do not praise. If the plan is sound, say so.

Tag every objection by how you know it:
  verified  = you read it in this codebase; evidence must be a real file:line
  plausible = follows from what you read, but you did not see the failure; evidence is the file:line it follows from
  guess     = a suspicion you could not check; evidence: none
Never present a guess as verified. Never cite a file you did not open. At most 5 objections, most serious first.

Reply in EXACTLY this format:
OBJECTIONS: NONE
or
OBJECTIONS:
1. [verified|plausible|guess] <what could go wrong or be lost, and why it matters> | evidence: <file:line, or none>
";

#[derive(Clone, Debug)]
pub struct Objection {
    pub confidence: String,
    pub text: String,
    pub evidence: String,
}

pub fn is_risky(request: &str) -> bool {
    risky().is_match(request)
}

pub fn parse_objections(text: &str) -> Vec<Objection> {
    let re = lit_i(r"(?m)^\s*\d+\.\s*\[(verified|plausible|guess)\]\s*(.+?)\s*\|\s*evidence:\s*(.*)$");
    let mut out = vec![];
    let mut at = 0;
    while let Some(c) = re.captures(&text[at..]) {
        let (_, e) = re.find(&text[at..]).unwrap();
        out.push(Objection { confidence: c[1].clone().unwrap_or_default().to_lowercase(), text: c[2].clone().unwrap_or_default().trim().into(), evidence: c[3].clone().unwrap_or_default().trim().into() });
        at += e.max(1);
    }
    out
}

/// Cite or drop. Returns the survivors and how many were removed.
pub async fn keep_grounded(objections: Vec<Objection>, cwd: &Path) -> (Vec<Objection>, usize) {
    let (mut kept, mut dropped) = (vec![], 0);
    for mut o in objections {
        let has_ref = !extract_refs(&o.evidence).is_empty();
        let problems = if has_ref { verify_references(&o.evidence, cwd).await.1 } else { vec![] };
        if !problems.is_empty() || (o.confidence == "verified" && !has_ref) {
            dropped += 1; // a made-up file, or a "verified" claim with nothing to check
            continue;
        }
        if o.confidence == "plausible" && !has_ref {
            o.confidence = "guess".into(); // no citation means it is only a suspicion
        }
        kept.push(o);
    }
    (kept, dropped)
}

pub fn render(objections: &[Objection], dropped: usize) -> String {
    let mut rows: Vec<String> = objections.iter().enumerate().map(|(i, o)| format!("{}. [{}] {} (evidence: {})", i + 1, o.confidence, o.text, if o.evidence.is_empty() { "none" } else { &o.evidence })).collect();
    if dropped > 0 {
        rows.push(format!("({dropped} more removed: the file or line they cited does not exist)"));
    }
    rows.join("\n")
}

/// (grounded objections, dropped count, error). Never fails: a broken challenger must not block the task.
pub async fn challenge_request(config: &Config, request: &str) -> (Vec<Objection>, usize, String) {
    let mut cfg = config.clone();
    cfg.model_role = "challenge".into();
    if !cfg.routing.contains_key("challenge") {
        cfg.routing.insert("challenge".into(), vec![CHALLENGER.into()]);
    }
    cfg.model_pinned = false;
    cfg.allowed_tools = Some(["read_file", "grep", "search", "outline", "git", "glob", "list_dir"].iter().map(|s| s.to_string()).collect());
    cfg.auto_review = false;
    cfg.auto_verify = false;
    cfg.auto_scout = false;
    cfg.auto_challenge = false;
    cfg.verify_commands = vec![];
    cfg.verify_rules = vec![];
    cfg.subagents = vec![];
    cfg.max_turns = 8;
    cfg.max_seconds = 180.0;
    cfg.approval = ApprovalPolicy::Never;
    let prompt = PROMPT.replace("{request}", &request.chars().take(4000).collect::<String>());
    let (texts, error) = crate::agent::agent::run_text(cfg, &prompt).await;
    let (kept, dropped) = keep_grounded(parse_objections(&texts.join("\n")), &config.cwd).await;
    (kept, dropped, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses() {
        let o = parse_objections("OBJECTIONS:\n1. [verified] drops the index | evidence: db/schema.sql:41\n2. [guess] old clients | evidence: none");
        assert_eq!(o.len(), 2);
        assert_eq!(o[0].confidence, "verified");
        assert!(is_risky("should we migrate to X"));
    }
}
