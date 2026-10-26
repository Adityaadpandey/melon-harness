//! Ranked code search: BM25 over file paths and contents. Identifiers are split at camelCase and snake_case
//! boundaries, stop words dropped, plurals stemmed, then files are ranked with Okapi BM25. A path hit counts three
//! times, since file names say what a file is for.
use crate::util::re::{lit, Re};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const STOP: &[&str] = &["a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on", "or", "that", "the", "this", "to", "with", "when", "if", "no", "not", "we", "do", "does"];
const MAX_FILE_BYTES: u64 = 200_000;
const SKIP_SUFFIX: &[&str] = &[".lock", ".svg", ".map", ".min.js", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".woff", ".woff2", ".pdf", ".zip", ".snap", ".csv"];
const PATH_WEIGHT: u32 = 3;
const NOISE_FACTOR: f64 = 0.35;

fn noise() -> &'static Re {
    // docs, specs, one-off scripts and tests describe code but are rarely the thing to fix
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"(^|/)(graphify-out|infrastructure|docs?|specs?|scripts|blog|content|fixtures|__tests__|tests?|unit|e2e|migrations)/|\.(md|mdx|spec\.\w+|test\.\w+)$"))
}

fn stem(t: &str) -> String {
    let n = t.chars().count();
    if n > 4 && t.ends_with("ies") {
        return format!("{}y", &t[..t.len() - 3]);
    }
    if n > 4 && ["ches", "shes", "sses", "xes", "zes"].iter().any(|s| t.ends_with(s)) {
        return t[..t.len() - 2].to_string();
    }
    if n > 3 && t.ends_with('s') && !t.ends_with("ss") {
        return t[..t.len() - 1].to_string();
    }
    t.to_string()
}

pub fn tokenize(text: &str) -> Vec<String> {
    // lower->Upper and ACRONYMWord boundaries become spaces before lowercasing
    let mut spaced = String::with_capacity(text.len() + 8);
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let p = chars[i - 1];
            let next_lower = chars.get(i + 1).map_or(false, |n| n.is_lowercase());
            if c.is_uppercase() && (p.is_lowercase() || p.is_ascii_digit() || (p.is_uppercase() && next_lower)) {
                spaced.push(' ');
            }
        }
        spaced.push(c);
    }
    spaced
        .to_lowercase()
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|t| !t.is_empty() && !STOP.contains(t))
        .map(stem)
        .collect()
}

struct Entry {
    mtime: std::time::SystemTime,
    counts: HashMap<String, u32>,
    length: u32,
}

fn cache() -> &'static Mutex<HashMap<(String, String), std::sync::Arc<Entry>>> {
    static C: OnceLock<Mutex<HashMap<(String, String), std::sync::Arc<Entry>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Term counts for one file, cached by mtime.
fn index(root: &Path, f: &Path) -> Option<std::sync::Arc<Entry>> {
    let key = (root.to_string_lossy().to_string(), f.to_string_lossy().to_string());
    let md = std::fs::metadata(f).ok()?;
    let mtime = md.modified().ok()?;
    if let Some(hit) = cache().lock().unwrap().get(&key) {
        if hit.mtime == mtime {
            return Some(hit.clone());
        }
    }
    let name = f.to_string_lossy();
    if SKIP_SUFFIX.iter().any(|s| name.ends_with(s)) || md.len() > MAX_FILE_BYTES {
        return None;
    }
    let raw = std::fs::read(f).ok()?;
    if raw[..raw.len().min(2048)].contains(&0) {
        return None;
    }
    let mut counts: HashMap<String, u32> = HashMap::new();
    for t in tokenize(&String::from_utf8_lossy(&raw)) {
        *counts.entry(t).or_default() += 1;
    }
    let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().replace('\\', "/");
    for t in tokenize(&rel) {
        *counts.entry(t).or_default() += PATH_WEIGHT;
    }
    let length = counts.values().sum();
    let e = std::sync::Arc::new(Entry { mtime, counts, length });
    cache().lock().unwrap().insert(key, e.clone());
    Some(e)
}

pub struct Hit {
    pub path: String,
    pub score: f64,
    pub snippets: Vec<(usize, String)>,
}

pub fn rank_files(root: &Path, files: &[PathBuf], query: &str, limit: usize, snippets: usize, prior: Option<&HashMap<String, f64>>) -> Vec<Hit> {
    let mut q = tokenize(query);
    let mut seen = std::collections::HashSet::new();
    q.retain(|t| seen.insert(t.clone()));
    if q.is_empty() {
        return vec![];
    }
    let docs: Vec<(&PathBuf, std::sync::Arc<Entry>)> = files.iter().filter_map(|f| index(root, f).map(|e| (f, e))).collect();
    if docs.is_empty() {
        return vec![];
    }
    let avg = (docs.iter().map(|(_, e)| e.length as f64).sum::<f64>() / docs.len() as f64).max(1.0);
    let n_docs = docs.len() as f64;
    let mut idf: Vec<(&String, f64)> = vec![];
    for t in &q {
        let n = docs.iter().filter(|(_, e)| e.counts.contains_key(t)).count() as f64;
        if n > 0.0 {
            idf.push((t, (1.0 + (n_docs - n + 0.5) / (n + 0.5)).ln()));
        }
    }
    let (k1, b) = (1.2, 0.75);
    let mut scored: Vec<(f64, &PathBuf)> = vec![];
    for (f, e) in &docs {
        let mut s = 0.0;
        for (t, w) in &idf {
            if let Some(c) = e.counts.get(*t) {
                let c = *c as f64;
                s += w * c * (k1 + 1.0) / (c + k1 * (1.0 - b + b * e.length as f64 / avg));
            }
        }
        if s > 0.0 {
            // files that match more distinct query terms beat files that repeat one
            s *= 1.0 + 0.5 * idf.iter().filter(|(t, _)| e.counts.contains_key(*t)).count() as f64;
            let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().replace('\\', "/");
            if noise().is_match(&rel) {
                s *= NOISE_FACTOR;
            }
            scored.push((s, f));
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    // rerank the leaders by co-occurrence: a line that holds several query terms says more than the same terms spread over a file
    let mut out: Vec<(f64, &PathBuf, Vec<(f64, usize)>, Vec<String>)> = vec![];
    for (s, f) in scored.into_iter().take(30) {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let lines: Vec<String> = text.lines().map(String::from).collect();
        let mut per_line: Vec<(f64, usize)> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let toks: std::collections::HashSet<String> = tokenize(l).into_iter().collect();
                (idf.iter().filter(|(t, _)| toks.contains(*t)).map(|(_, w)| w).sum::<f64>(), i)
            })
            .collect();
        per_line.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
        let top: Vec<(f64, usize)> = per_line.into_iter().take(snippets).filter(|x| x.0 > 0.0).collect();
        let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().replace('\\', "/");
        let boost = 1.0 + prior.and_then(|p| p.get(&rel)).copied().unwrap_or(0.0); // files that fixed similar requests before
        let sc = s * (1.0 + 0.15 * top.iter().map(|x| x.0).sum::<f64>()) * boost;
        out.push((sc, f, top, lines));
    }
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out.into_iter()
        .take(limit)
        .map(|(s, f, mut top, lines)| {
            top.sort_by_key(|x| x.1);
            Hit {
                path: f.strip_prefix(root).unwrap_or(f).to_string_lossy().replace('\\', "/"),
                score: (s * 10.0).round() / 10.0,
                snippets: top.iter().map(|(_, i)| (i + 1, crate::util::text::head_chars(lines[*i].trim(), 160))).collect(),
            }
        })
        .collect()
}

/// Build the index for a whole repo in a background thread at startup, so the first `search` finds it ready.
pub fn warm(root: PathBuf, forbidden: Vec<String>) {
    std::thread::spawn(move || {
        for f in crate::util::search::list_files(&root, &forbidden) {
            let _ = index(&root, &f);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens() {
        assert_eq!(tokenize("EventAbout.tsx"), vec!["event", "about", "tsx"]);
        assert_eq!(tokenize("parse_HTTPResponse"), vec!["parse", "http", "response"]);
        assert_eq!(tokenize("classes of the entries"), vec!["class", "entry"]);
    }
}
