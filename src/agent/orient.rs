//! Understand a request before touching the repo: what kind of job it is, how hard, and where in THIS repo the answer
//! probably lives. One small call to the fast model reads the request next to a compact map of the repo and answers in
//! JSON. Every part is optional: when no fast model is configured, or the call fails, the agent falls back to regex rules.
use crate::client::llm_client::LLMClient;
use crate::client::response::StreamEvent;
use crate::util::search::list_files;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

const MAP_CHARS: usize = 3500; // about 900 tokens: the whole map rides along with every request, so it stays small

#[derive(Clone, Debug)]
pub struct Orientation {
    pub intent: String,
    pub tier: &'static str,
    /// What the change should land in: code, docs, config or none.
    pub touches: String,
    pub where_: Vec<String>,
    pub queries: Vec<String>,
    pub note: String,
}

impl Orientation {
    /// The few lines the main agent gets. A guess from a map, so it is told to confirm before relying on it.
    pub fn hint(&self) -> String {
        if self.where_.is_empty() && self.queries.is_empty() {
            return String::new();
        }
        let mut parts = vec![];
        if !self.where_.is_empty() {
            parts.push(format!("likely in {}", self.where_.iter().take(5).cloned().collect::<Vec<_>>().join(", ")));
        }
        if !self.queries.is_empty() {
            parts.push(format!("start with search for {}", self.queries.iter().take(3).map(|q| format!("'{q}'")).collect::<Vec<_>>().join(" / ")));
        }
        format!("[Orientation, a guess from a map of the repo (confirm by reading before relying on it): {}{}]", parts.join("; "), if self.note.is_empty() { String::new() } else { format!(". {}", self.note) })
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    std::process::Command::new("git").args(args).current_dir(cwd).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

/// Folders (two levels deep) with their file counts, the top-level files, what the last commits touched and what is uncommitted.
pub fn repo_map(cwd: &Path, forbidden: &[String]) -> String {
    let files = list_files(cwd, forbidden);
    let mut folders: BTreeMap<String, usize> = BTreeMap::new();
    let mut top: Vec<String> = vec![];
    for f in files {
        let rel = f.strip_prefix(cwd).unwrap_or(&f).to_string_lossy().replace('\\', "/");
        let parts: Vec<&str> = rel.split('/').collect();
        if parts.len() == 1 {
            top.push(rel);
        } else {
            let key = if parts.len() > 2 { format!("{}/{}", parts[0], parts[1]) } else { parts[0].to_string() };
            *folders.entry(key).or_default() += 1;
        }
    }
    let mut counted: Vec<(&String, &usize)> = folders.iter().filter(|(_, n)| **n > 0).collect();
    counted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let tree = counted.iter().take(40).map(|(d, n)| format!("{d}/ ({n})")).collect::<Vec<_>>().join(", ");
    let mut recent: Vec<String> = vec![];
    for l in git(cwd, &["log", "-8", "--name-only", "--pretty=format:"]).lines().filter(|l| !l.trim().is_empty()) {
        if !recent.iter().any(|r| r == l) {
            recent.push(l.to_string());
        }
    }
    recent.truncate(14);
    let dirty: Vec<String> = git(cwd, &["status", "--short", "-uall"]).lines().map(|l| l.get(3..).unwrap_or("").to_string()).take(10).collect();
    let text = format!(
        "FOLDERS: {tree}\nROOT FILES: {}\nRECENTLY CHANGED: {}\nUNCOMMITTED: {}",
        top.iter().take(20).cloned().collect::<Vec<_>>().join(", "),
        if recent.is_empty() { "(none)".into() } else { recent.join(", ") },
        if dirty.is_empty() { "(clean)".into() } else { dirty.join(", ") }
    );
    text.chars().take(MAP_CHARS).collect()
}

const PROMPT: &str = r#"You help a coding agent get oriented before it works. Read the request and the map of the repo, then reply with ONE JSON object and nothing else.

REQUEST: {request}

REPO MAP:
{repo}

Fields:
"intent": "change" (the user wants files edited or created), "review" (check code, a diff or a PR and report), "investigate" (find a cause or trace how something works), "explain" (answer a question about the code), "git" (commit, branch, push, status), or "other".
  Judge by what the user wants done, not by single words: "review the change i have" is a review; "why does checkout double charge" is investigate even though nothing is named.
"tier": "fast" (trivial and well-defined: rename, typo, where is X, run a command), "default" (ordinary work), or "strong" (hard: a bug with an unknown cause, concurrency, security or money paths, a change across several parts, a design decision).
"touches": what the finished work should change: "code" (source files: a bug fix or feature), "docs" (markdown, comments or notes only), "config" (CI, build, env or infra files), or "none" (nothing is edited).
"where": up to 5 paths or folders from the map where the answer most likely lives (only ones that appear in the map; [] if you cannot tell).
"queries": up to 3 plain-word search queries that would find the relevant code (identifiers, domain words the user used, synonyms a developer would have used in code); [] if the request names exact files.
"note": at most one short sentence: the one thing worth knowing before starting (for example that the uncommitted change is what the user means), or "".
"#;

pub fn parse(reply: &str) -> Option<Orientation> {
    let (s, e) = (reply.find('{')?, reply.rfind('}')?);
    let d: Value = serde_json::from_str(&reply[s..=e]).ok()?;
    if !d.is_object() {
        return None;
    }
    let pick = |k: &str, ok: &[&str], dflt: &str| d[k].as_str().filter(|v| ok.contains(v)).unwrap_or(dflt).to_string();
    let intent = pick("intent", &["change", "review", "investigate", "explain", "git", "other"], "other");
    let tier = match d["tier"].as_str() {
        Some("fast") => "fast",
        Some("strong") => "strong",
        _ => "default",
    };
    let clean = |k: &str| -> Vec<String> { d[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.chars().take(120).collect())).take(5).collect()).unwrap_or_default() };
    let mut queries = clean("queries");
    queries.truncate(3);
    Some(Orientation { intent, tier, touches: pick("touches", &["code", "docs", "config", "none"], "none"), where_: clean("where"), queries, note: d["note"].as_str().unwrap_or("").chars().take(200).collect() })
}

/// None when the call fails or the answer is not usable: the caller keeps its regex rules.
pub async fn orient(client: &Arc<LLMClient>, role: &str, request: &str, repo: &str) -> Option<Orientation> {
    let prompt = PROMPT.replace("{request}", &request.chars().take(1500).collect::<String>()).replace("{repo}", repo);
    let mut text = String::new();
    let mut s = client.chat_completion(vec![json!({"role": "user", "content": prompt})], None, false, role);
    while let Some(ev) = s.next().await {
        match ev {
            StreamEvent::MessageComplete { text: Some(t), .. } => text.push_str(&t),
            StreamEvent::Error(_) => return None,
            _ => {}
        }
    }
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_loose_json() {
        let o = parse("sure: {\"intent\": \"review\", \"tier\": \"strong\", \"touches\": \"none\", \"where\": [\"src/\"], \"queries\": [\"a\", \"b\", \"c\", \"d\"], \"note\": \"x\"}").unwrap();
        assert_eq!((o.intent.as_str(), o.tier, o.queries.len()), ("review", "strong", 3));
        assert!(o.hint().contains("likely in src/"));
        assert!(parse("no json").is_none());
        assert_eq!(parse("{\"intent\": \"zzz\"}").unwrap().intent, "other");
    }
}
