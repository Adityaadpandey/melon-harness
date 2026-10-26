//! Deterministic hallucination check for the final answer. Every file path (and file:line) the model cites is checked
//! against the real tree: a path that does not exist, or a line past the end of the file, is a fabricated reference.
use crate::agent::routing_policy::reviewy;
use crate::util::re::{lit, lit_i, Re};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::process::Command;

const EXTS: &str = "ts|tsx|js|jsx|mjs|cjs|py|json|md|yml|yaml|toml|sql|prisma|css|scss|html|sh|go|rs|java|kt|swift|ejs|conf|tf|rb|php|c|h|cpp|vue|svelte";
const MAX_REFS: usize = 40;

#[derive(Clone, Debug)]
pub struct Problem {
    pub ref_: String,
    pub why: String,
}

fn ref_re() -> &'static Re {
    // path/with.ext  or  file.ext:12  or  file.ext:12-30   (not URLs, not versions)
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| Re::new(&format!(r"(?<![\w/:.@-])((?:[\w.@\-]+/)*[\w.@\-]+\.(?:{EXTS}))(?::(\d+)(?:[-–](\d+))?)?(?![\w/])"), false).unwrap())
}

pub type Ref = (String, Option<usize>, Option<usize>);

pub fn extract_refs(text: &str) -> Vec<Ref> {
    let text = lit(r"https?://\S+").replace_all(text, "").to_string();
    let (mut seen, mut out): (std::collections::HashSet<Ref>, Vec<Ref>) = (Default::default(), vec![]);
    let mut at = 0;
    while at < text.len() {
        let Some(c) = ref_re().captures(&text[at..]) else { break };
        let (_, e) = ref_re().find(&text[at..]).unwrap();
        let path = c[1].clone().unwrap_or_default();
        let a = c.get(2).cloned().flatten().and_then(|x| x.parse().ok());
        let b = c.get(3).cloned().flatten().and_then(|x| x.parse().ok());
        at += e.max(1);
        if !path.contains('/') && a.is_none() {
            continue; // a bare name like `index.ts` is too ambiguous to judge
        }
        let key = (path, a, b);
        if seen.insert(key.clone()) {
            out.push(key);
        }
    }
    out.truncate(MAX_REFS);
    out
}

async fn git_files(cwd: &Path) -> Option<Vec<String>> {
    let o = Command::new("git").args(["ls-files", "-co", "--exclude-standard"]).current_dir(cwd).output().await.ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
}

pub async fn tree(cwd: &Path) -> Vec<String> {
    if let Some(f) = git_files(cwd).await {
        return f;
    }
    // started in a folder that holds repos: list each repo one level down, so "apps/server/x.ts" still resolves
    let mut out = vec![];
    for d in crate::agent::root::child_repos(cwd) {
        if let Some(fs) = git_files(&d).await {
            let name = d.file_name().unwrap().to_string_lossy().to_string();
            out.extend(fs.into_iter().map(|f| format!("{name}/{f}")));
        }
    }
    out
}

pub fn resolve_in_tree(cwd: &Path, r: &str, tree: &[String]) -> Option<PathBuf> {
    let direct = cwd.join(r);
    if direct.is_file() {
        return Some(direct);
    }
    // models cite paths relative to a sub-app (src/app.ts for apps/server/src/app.ts)
    tree.iter().find(|t| *t == r || t.ends_with(&format!("/{r}"))).map(|t| cwd.join(t))
}

pub async fn verify_references(text: &str, cwd: &Path) -> (usize, Vec<Problem>) {
    let refs = extract_refs(text);
    if refs.is_empty() {
        return (0, vec![]);
    }
    let tree = tree(cwd).await;
    let mut problems = vec![];
    for (path, a, b) in &refs {
        let label = format!("{path}{}{}", a.map(|a| format!(":{a}")).unwrap_or_default(), b.map(|b| format!("-{b}")).unwrap_or_default());
        let Some(f) = resolve_in_tree(cwd, path, &tree) else {
            problems.push(Problem { ref_: label, why: "no such file in the workspace".into() });
            continue;
        };
        if let Some(a) = a {
            let Ok(bytes) = std::fs::read(&f) else { continue };
            let n = bytes.iter().filter(|b| **b == b'\n').count() + usize::from(bytes.last().map_or(false, |l| *l != b'\n'));
            if *a > n || b.map_or(false, |b| b > n + 1) {
                problems.push(Problem { ref_: label, why: format!("file has only {n} lines") });
            }
        }
    }
    (refs.len(), problems)
}

pub fn correction_prompt(problems: &[Problem]) -> String {
    let rows = problems.iter().take(15).map(|p| format!("- {}: {}", p.ref_, p.why)).collect::<Vec<_>>().join("\n");
    format!("Automatic check: these references in your last answer do not exist:\n{rows}\n\nRe-check each with your tools (grep/read_file) and correct it, or remove the claim. Do not guess a replacement. Reply with the corrected answer only.")
}

fn edit_verbs() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(fix|add|implement|change|update|refactor|rename|write|create|remove|delete|replace|make|move|extract|split|convert|migrate|rewrite|patch|build|set up|wire)\b"))
}
fn readonly() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(read[- ]only|do not (modify|change|edit)|don't (modify|change|edit)|without (modifying|changing)|no (changes|edits)|just (explain|tell|show|answer)|investigate|explain|what is|where is|why)\b"))
}
fn fix_asked() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(fix|patch|apply|implement)\b"))
}
const SHELLISH: &[&str] = &["bash", "sh", "shell", "zsh", "console", "sql", "text", "txt", "output", "log", "diff", "http", "curl"];

/// Does the request ask for a change to files? Read-only questions must not be nudged to write. "review the change i
/// have" is a review: the word "change" there is the thing reviewed, not a verb. Only "review and fix" edits.
pub fn wants_change(request: &str) -> bool {
    if reviewy().is_match(request) && !fix_asked().is_match(request) {
        return false;
    }
    edit_verbs().is_match(request) && !readonly().is_match(request)
}

/// A fenced block in a source language (not a command or output the user is meant to run or read).
pub fn has_file_code(answer: &str) -> bool {
    let mut langs: Vec<String> = vec![];
    let mut at = 0;
    while let Some(i) = answer[at..].find("```") {
        let rest = &answer[at + i + 3..];
        let lang: String = rest.chars().take_while(|c| c.is_alphanumeric() || "+#-_".contains(*c)).collect();
        langs.push(lang);
        at += i + 3;
    }
    let evens: Vec<&String> = langs.iter().step_by(2).collect();
    let pool: Vec<&String> = if evens.is_empty() { langs.iter().collect() } else { evens };
    pool.iter().any(|l| !SHELLISH.contains(&l.to_lowercase().as_str()))
}

fn second_thoughts() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| {
        lit_i(r"(?m)non-issue|not an? (real )?(bug|issue|problem|defect|regression)|no (defect|regression|issue here)|\bskip\b\.?|acceptable[;.,]|actually (fine|ok|correct)|coincidence|no collision here|\bverified (correct|fine)|pre-existing behaviou?r|nothing to fix|\bretract|\bmoot\b|no fix needed|^\W*final answer\b")
    })
}

/// Words: a finding is two lines (`severity file:line  what -> fix`); more than this is the model thinking out loud.
const LONGEST_FINDING: usize = 55;

fn finding_start() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"(?m)^\W*(?:HIGH|MEDIUM|LOW|CRITICAL)\b"))
}

/// Word count of each severity-labelled item, up to the next one (or a blank line).
pub fn finding_lengths(answer: &str) -> Vec<usize> {
    let mut starts = vec![];
    let mut at = 0;
    while let Some((s, e)) = finding_start().find(&answer[at..]) {
        starts.push(at + s);
        at += e.max(s + 1);
    }
    let mut out = vec![];
    for (i, s) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(answer.len());
        let chunk = &answer[*s..end];
        let first = chunk.split("\n\n").next().unwrap_or(chunk);
        out.push(first.split_whitespace().count());
    }
    out
}

/// A review that lists a finding and then talks itself out of it has put its scratch thinking in the answer.
pub fn shows_second_thoughts(request: &str, answer: &str) -> bool {
    if !lit_i(r"\breview").is_match(request) {
        return false;
    }
    second_thoughts().is_match(answer) || finding_lengths(answer).iter().any(|n| *n > LONGEST_FINDING)
}

pub const REWRITE_PROMPT: &str = "Your review lists findings you then dismissed or argued about. Rewrite it from scratch: delete every dismissed item and every redundancy, duplicate-lookup or style item. Keep only defects you can point to as `severity file:line  what is wrong -> fix`, at most 6, two lines each, no deliberation. Rank by what the user sees: a new awaited call or write on a request path whose failure now fails the whole response goes first. HIGH only for what you verified by reading the code; a guess about a library, an admin or edge path, or a performance cost is `(unverified)` and at most MEDIUM, and drop it if one grep or read could settle it. A flag or option the diff adds on purpose is not a defect unless you can name what breaks. Reply with the rewritten review only.";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refs_and_intent() {
        let r = extract_refs("see src/a.ts:12-30 and `index.ts` and https://x.com/a/b.ts and lib/b.py");
        assert_eq!(r, vec![("src/a.ts".to_string(), Some(12), Some(30)), ("lib/b.py".to_string(), None, None)]);
        assert!(wants_change("fix the login bug"));
        assert!(!wants_change("review the change i have"));
        assert!(wants_change("review and fix the change"));
        assert!(!wants_change("explain how this works"));
        assert!(has_file_code("```python\nx=1\n```"));
        assert!(!has_file_code("```bash\nls\n```"));
        assert!(shows_second_thoughts("review it", "HIGH a.py:1 bug. Actually fine, skip."));
        assert!(!shows_second_thoughts("fix it", "non-issue"));
    }
}
