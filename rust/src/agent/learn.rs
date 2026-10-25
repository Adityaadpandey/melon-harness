//! What melon learns about a repo between sessions, kept outside the repo (data dir). Only things that cannot be
//! hallucinated go in: lessons (what YOU told it) and edit history (request words -> the files a verified run changed).
//! Nothing the model merely claims is stored.
use crate::client::llm_client::LLMClient;
use crate::client::response::StreamEvent;
use crate::config::loader::get_data_dir;
use crate::util::paths::{atomic_write, resolve};
use crate::util::rank::tokenize;
use futures::StreamExt;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_LESSONS: usize = 30;
const MAX_EDITS: usize = 200;
const PROMPT_CHARS: usize = 1500;
/// Share of the search query's words a past request must have in common.
const MIN_OVERLAP: f64 = 0.5;
pub const HEADING: &str = "## Learned by melon";
const JUDGE_PROMPT: &str = "You watch a coding agent work with its user. Decide whether the user's NEW message teaches the agent something worth remembering for this repo.

Worth keeping:
- the user redirects or corrects the agent (\"no, not that\", \"I wanted X\", changing their mind, giving something other than what the agent did)
- the user confirms an approach was right (\"yes exactly like that\", \"perfect, keep doing it this way\")
- the user states a preference or convention, however it is phrased

Not worth keeping: a new unrelated task, a question, a one-off detail that will not matter next time, thanks, small talk.

Reply with ONE short rule in the imperative that would help next time, or nothing at all. Say what to do and, when the correction shows it, what not to do. Example:
  agent built a dark mode toggle, user: \"no, follow the system theme instead\" -> Dark mode follows the system theme; do not add a manual toggle.
Reply with only the rule, or the single word NONE.";

fn squash(s: &str, n: usize) -> String {
    crate::util::text::head_chars(&crate::util::text::squash(s), n)
}

/// Ask a model whether `message` (the user's reply) is a correction, a confirmation or a preference worth keeping.
/// Returns the rule to save, or "" when there is nothing to learn. Never fails: learning must not hurt a run.
pub async fn judge(client: &Arc<LLMClient>, before: &str, answer: &str, message: &str) -> String {
    let ask = format!("The user asked: {}\nThe agent answered: {}\nThe user's NEW message: {}", squash(before, 400), squash(answer, 600), squash(message, 400));
    let mut out = String::new();
    let mut s = client.chat_completion(vec![json!({"role": "system", "content": JUDGE_PROMPT}), json!({"role": "user", "content": ask})], None, false, "summarize");
    while let Some(ev) = s.next().await {
        match ev {
            StreamEvent::MessageComplete { text: Some(t), .. } => out.push_str(&t),
            StreamEvent::Error(_) => return String::new(),
            _ => {}
        }
    }
    let rule = crate::util::text::squash(&out);
    let rule = rule.trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == ' ').to_string();
    if rule.is_empty() || rule.to_uppercase().starts_with("NONE") || rule.chars().count() > 300 { String::new() } else { rule }
}

pub struct Learned {
    root: PathBuf,
    path: PathBuf,
    data: Value,
}

impl Learned {
    pub fn new(root: &Path) -> Learned {
        Self::with_base(root, &get_data_dir())
    }

    pub fn with_base(root: &Path, base: &Path) -> Learned {
        let root = resolve(root);
        let h = hex::encode(Sha1::digest(root.to_string_lossy().as_bytes()));
        let path = base.join("learned").join(format!("{}.json", &h[..16]));
        let mut data = json!({"lessons": [], "edits": []});
        if let Some(loaded) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            for k in ["lessons", "edits"] {
                data[k] = loaded.get(k).filter(|v| v.is_array()).cloned().unwrap_or(json!([]));
            }
        }
        Learned { root, path, data }
    }

    fn save(&self) {
        if let Some(d) = self.path.parent() {
            if std::fs::create_dir_all(d).is_ok() {
                let _ = atomic_write(&self.path, &serde_json::to_string_pretty(&self.data).unwrap_or_default()); // learning is a bonus: never fail a run over it
            }
        }
    }

    pub fn add_lesson(&mut self, text: &str, source: &str) -> bool {
        let text: String = crate::util::text::squash(text).chars().take(300).collect();
        let exists = self.data["lessons"].as_array().unwrap().iter().any(|l| l["text"].as_str().unwrap_or("").to_lowercase() == text.to_lowercase());
        if text.split_whitespace().count() < 4 || exists {
            return false;
        }
        let in_file = self.to_agents_md(&text);
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let lessons = self.data["lessons"].as_array_mut().unwrap();
        lessons.push(json!({"text": text, "source": source, "ts": ts, "in_file": in_file}));
        if lessons.len() > MAX_LESSONS {
            let drop = lessons.len() - MAX_LESSONS;
            lessons.drain(..drop);
        }
        self.save();
        true
    }

    /// Also write the lesson into the repo's AGENTS.md under '## Learned by melon', so the team gets it too and the next
    /// session reads it as an ordinary instruction. False when the file can't be written.
    fn to_agents_md(&self, text: &str) -> bool {
        let f = self.root.join("AGENTS.md");
        let mut body = if f.is_file() { std::fs::read_to_string(&f).unwrap_or_default() } else { String::new() };
        if !body.contains(HEADING) {
            body = format!("{}{HEADING}\n", if body.trim().is_empty() { String::new() } else { format!("{}\n\n", body.trim_end_matches('\n')) });
        }
        std::fs::write(&f, format!("{}\n- {text}\n", body.trim_end_matches('\n'))).is_ok()
    }

    fn out_of_agents_md(&self, text: &str) {
        let f = self.root.join("AGENTS.md");
        if let Ok(body) = std::fs::read_to_string(&f) {
            let _ = std::fs::write(&f, body.replacen(&format!("- {text}\n"), "", 1));
        }
    }

    pub fn forget(&mut self, index: usize) -> Option<String> {
        let lessons = self.data["lessons"].as_array_mut().unwrap();
        if index >= 1 && index <= lessons.len() {
            let gone = lessons.remove(index - 1);
            let text = gone["text"].as_str().unwrap_or("").to_string();
            if gone["in_file"].as_bool().unwrap_or(false) {
                self.out_of_agents_md(&text);
            }
            self.save();
            return Some(text);
        }
        None
    }

    pub fn lessons(&self) -> Vec<String> {
        self.data["lessons"].as_array().unwrap().iter().map(|l| l["text"].as_str().unwrap_or("").to_string()).collect()
    }

    pub fn prompt_section(&self) -> Option<String> {
        let (mut out, mut used) = (vec![], 0usize);
        for l in self.data["lessons"].as_array().unwrap().iter().rev() {
            // newest first, so old ones fall off the cap
            let text = l["text"].as_str().unwrap_or("");
            if l["in_file"].as_bool().unwrap_or(false) {
                continue; // already in AGENTS.md, which the prompt carries
            }
            if used + text.chars().count() > PROMPT_CHARS {
                break;
            }
            out.push(format!("- {text}"));
            used += text.chars().count();
        }
        (!out.is_empty()).then(|| format!("# Learned in this repo\n\nThe user taught you these; follow them:\n{}", out.join("\n")))
    }

    /// Call only for runs that ended verified; `changed` are repo-relative paths.
    pub fn record_run(&mut self, request: &str, changed: &[String]) {
        let mut seen = HashSet::new();
        let terms: Vec<String> = tokenize(request).into_iter().filter(|t| seen.insert(t.clone())).take(40).collect(); // first-said words carry the subject of a long brief
        let files: Vec<&String> = changed.iter().filter(|f| self.root.join(f).is_file()).take(8).collect();
        let dup = self.data["edits"].as_array().unwrap().iter().any(|e| e["terms"] == json!(terms) && e["files"] == json!(files));
        if terms.len() < 3 || files.is_empty() || dup {
            return;
        }
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let edits = self.data["edits"].as_array_mut().unwrap();
        edits.push(json!({"terms": terms, "files": files, "ts": ts}));
        if edits.len() > MAX_EDITS {
            let drop = edits.len() - MAX_EDITS;
            edits.drain(..drop);
        }
        self.save();
    }

    /// File -> boost (0..1) from past verified runs whose request resembled this query.
    pub fn prior(&self, query: &str) -> HashMap<String, f64> {
        let q: HashSet<String> = tokenize(query).into_iter().collect();
        let mut boosts: HashMap<String, f64> = HashMap::new();
        if q.is_empty() {
            return boosts;
        }
        for e in self.data["edits"].as_array().unwrap() {
            let t: HashSet<String> = e["terms"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            // share of the query's words, not of both: a 40-word brief would otherwise drown a 5-word search
            let overlap = q.intersection(&t).count() as f64 / q.len() as f64;
            if overlap >= MIN_OVERLAP {
                for f in e["files"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    let b = boosts.entry(f.to_string()).or_insert(0.0);
                    *b = (*b + overlap).min(1.0);
                }
            }
        }
        boosts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lessons_and_priors() {
        let dir = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("a.py"), "x").unwrap();
        let mut l = Learned::with_base(repo.path(), dir.path());
        assert!(!l.add_lesson("too short", "user"));
        assert!(l.add_lesson("Always run the linter before committing code", "user"));
        assert!(!l.add_lesson("always run the linter before committing code", "user"));
        assert!(std::fs::read_to_string(repo.path().join("AGENTS.md")).unwrap().contains("- Always run the linter"));
        l.record_run("fix the description blank lines on the event page", &["a.py".into()]);
        let l2 = Learned::with_base(repo.path(), dir.path());
        assert_eq!(l2.lessons().len(), 1);
        assert!(l2.prior("event description blank lines").contains_key("a.py"));
        let mut l2 = l2;
        assert!(l2.forget(1).is_some());
        assert!(!std::fs::read_to_string(repo.path().join("AGENTS.md")).unwrap().contains("Always run"));
    }
}
