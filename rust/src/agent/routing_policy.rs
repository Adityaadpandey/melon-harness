//! Pick a model tier for a request without spending a model call.
//!   fast    - small, well-defined work (rename, typo, where is X, explain this)
//!   default - the configured main model
//!   strong  - debugging, concurrency, security, architecture, multi-file work
//! The classifier is deliberately simple and explainable (it returns its reason), and the agent escalates to `strong`
//! on evidence of trouble (a failed repair, a detected loop) rather than guessing up front.
use crate::util::re::{lit, lit_i, Re};
use std::sync::OnceLock;

fn strong() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(debug\w*|root cause|race condition|concurren\w*|deadlock|refactor\w*|architect\w*|migrat\w*|security|vulnerab\w*|auth\w*|payment\w*|webhook\w*|performance|optimi[sz]\w*|intermittent|flaky|regression|why (does|is|are|do)|memory leak|transaction\w*|idempoten\w*|design|investigate)\b"))
}
fn fast() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(rename|typo|comment|format\w*|lint|where (is|are)|find|list|show|what (does|is)|explain|read|open|print|bump|add (a )?(log|import)|remove (the )?(unused|log))\b"))
}
fn pathy() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"[\w.\-]+/[\w./\-]+|\b[\w\-]+\.(?:ts|tsx|js|jsx|py|go|rs|java|sql|prisma)\b"))
}
pub fn reviewy() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(review|audit|critique)\b"))
}
/// "review it with one single agent", "a single reviewer", "without the pipeline": the main agent reviews it itself.
pub fn one_agent() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(?:(?:one|1|a)\s+)?single[- ](?:agent|reviewer)s?\b|\b(?:one|1)\s+(?:agent|reviewer)\b|\b(?:no|without)\s+(?:the\s+)?(?:sub-?agents?|pipeline|fan-?out|parallel reviewers)\b"))
}
fn explainy() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(explain|walk me through|how does|what does|summari[sz]e)\b"))
}

/// (tier, reason)
pub fn classify(request: &str) -> (&'static str, String) {
    let text = request.trim();
    if let Some((s, e)) = strong().find(text) {
        return ("strong", format!("'{}' suggests a hard task", text[s..e].to_lowercase()));
    }
    let n = text.chars().count();
    if n > 700 {
        return ("strong", "long, detailed request".into());
    }
    let mut paths = 0;
    let mut at = 0;
    while let Some((_, e)) = pathy().find(&text[at..]) {
        paths += 1;
        at += e.max(1);
    }
    if paths >= 4 {
        return ("strong", "touches several files".into());
    }
    if n < 200 && fast().is_match(text) && !lit_i(r"(?s)\band\b.*\band\b").is_match(text) {
        return ("fast", "small, well-defined request".into());
    }
    ("default", "ordinary task".into())
}

/// Tokens of context worth keeping cached.
const CACHE_WORTH_KEEPING: i64 = 20_000;

/// Switching model resets the provider's prompt cache, so the next call re-bills the whole context at full price. Past
/// CACHE_WORTH_KEEPING a request stays on the model in use; only evidence of trouble (a failed repair, a loop) escalates.
pub fn keep_cached_model(tier: &'static str, context_tokens: i64) -> &'static str {
    if tier != "default" && context_tokens >= CACHE_WORTH_KEEPING { "default" } else { tier }
}

/// fast work needs little thinking, hard work gets the most; "default" leaves the model's own setting alone.
pub fn effort(tier: &str) -> Option<&'static str> {
    match tier {
        "fast" => Some("low"),
        "strong" => Some("high"),
        _ => None,
    }
}

/// One line appended to the user's message telling the model how long the answer should be.
pub fn length_hint(request: &str, tier: &str, will_change: bool) -> String {
    if reviewy().is_match(request) {
        return "Answer with at most 6 findings, each `severity path:line  what is wrong -> fix`; if there is no defect, only: No issues found.".into();
    }
    if explainy().is_match(request) && !will_change {
        return "Answer in under 300 words, with file:line references.".into();
    }
    if tier == "fast" && will_change {
        return "When done, reply in 1-2 sentences: what changed.".into();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tiers() {
        assert_eq!(classify("debug the flaky test").0, "strong");
        assert_eq!(classify("rename foo to bar").0, "fast");
        assert_eq!(classify("add a button to the page").0, "default");
        assert_eq!(keep_cached_model("strong", 30_000), "default");
        assert!(one_agent().is_match("review it with one single agent"));
    }
}
