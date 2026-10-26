//! File discovery and text search shared by grep/glob. Discovery asks git first (`git ls-files -co --exclude-standard`),
//! so .gitignore is respected, dotfiles like .github/ are found, and node_modules/dist never flood the results. Outside
//! git it walks the tree with sensible skips. Files that look like secrets are never listed or searched.
use crate::safety::guard::is_forbidden_path;
use crate::util::fnmatch::fnmatch;
use crate::util::re::Re;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const SECRET_GLOBS: &[&str] = &[".env", ".env.*", "*.pem", "*.key", "id_rsa*", "id_ed25519*"];
/// graphify-out holds generated graph files (14.6k of Tunyt's 23.8k tracked files): they bury real matches and cost a stat each.
pub const SKIP_DIRS: &[&str] = &["node_modules", "__pycache__", ".git", ".venv", "venv", "dist", "build", ".next", ".turbo", "target", ".pytest_cache", "graphify-out", ".deepsec", "coverage"];
const MAX_FILE_BYTES: u64 = 2_000_000;
const MAX_LINE_CHARS: usize = 300;
const LIST_TTL: Duration = Duration::from_secs(20);

pub fn is_secret_name(name: &str) -> bool {
    is_secret(name)
}

fn is_secret(name: &str) -> bool {
    let low = name.to_lowercase();
    SECRET_GLOBS.iter().any(|g| fnmatch(&low, g))
}

type ListKey = (String, Vec<String>, u128);

struct Listing {
    cache: HashMap<ListKey, (Instant, Vec<PathBuf>)>,
    depth: usize,
}

fn listing() -> &'static Mutex<Listing> {
    static L: OnceLock<Mutex<Listing>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(Listing { cache: HashMap::new(), depth: 0 }))
}

/// The agent holds the listing while it runs one round of tool calls, so five parallel greps share one git ls-files
/// instead of paying it five times. Dropped when the round ends.
pub fn hold_listing(on: bool) {
    let mut l = listing().lock().unwrap();
    l.depth = if on { l.depth + 1 } else { l.depth.saturating_sub(1) };
    if l.depth == 0 {
        l.cache.clear();
    }
}

/// A write may have added or removed files: the next grep lists again.
pub fn forget_listing() {
    listing().lock().unwrap().cache.clear();
}

/// Every searchable file under root, in a stable sorted order.
pub fn list_files(root: &Path, forbidden: &[String]) -> Vec<PathBuf> {
    let stamp = std::fs::metadata(root.join(".git/index")).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let key: ListKey = (root.to_string_lossy().to_string(), forbidden.to_vec(), stamp);
    {
        let l = listing().lock().unwrap();
        if l.depth > 0 {
            if let Some((t, files)) = l.cache.get(&key) {
                if t.elapsed() < LIST_TTL {
                    return files.clone();
                }
            }
        }
    }
    let files = list_files_uncached(root, forbidden);
    let mut l = listing().lock().unwrap();
    if l.depth > 0 {
        l.cache.insert(key, (Instant::now(), files.clone()));
    }
    files
}

fn list_files_uncached(root: &Path, forbidden: &[String]) -> Vec<PathBuf> {
    let mut rels: Vec<String> = vec![];
    if let Ok(o) = std::process::Command::new("git").args(["ls-files", "-co", "--exclude-standard", "-z"]).current_dir(root).output() {
        if o.status.success() {
            rels = String::from_utf8_lossy(&o.stdout).split('\0').filter(|p| !p.is_empty()).map(String::from).collect();
        }
    }
    if rels.is_empty() {
        for e in walkdir::WalkDir::new(root).into_iter().filter_entry(|e| !(e.file_type().is_dir() && e.depth() > 0 && SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref()))).flatten() {
            if e.file_type().is_file() {
                if let Ok(r) = e.path().strip_prefix(root) {
                    rels.push(r.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    // names repeat a lot (23k files in a monorepo share a few hundred folder names), so each name is judged once
    let mut bad: HashMap<&str, bool> = HashMap::new();
    let mut out: Vec<String> = Vec::with_capacity(rels.len());
    for rel in &rels {
        let mut parts: Vec<&str> = rel.split('/').collect();
        let name = parts.pop().unwrap_or("");
        let dir_blocked = parts.iter().any(|d| *bad.entry(d).or_insert_with(|| SKIP_DIRS.contains(d) || is_forbidden_path(d, forbidden).is_some()));
        if is_secret(name) || dir_blocked || (!forbidden.is_empty() && is_forbidden_path(name, forbidden).is_some()) {
            continue;
        }
        match std::fs::metadata(root.join(rel)) {
            Ok(m) if m.is_file() && m.len() <= MAX_FILE_BYTES => out.push(rel.clone()),
            _ => {}
        }
    }
    // the order sorted() gives path objects: folder by folder
    out.sort_by(|a, b| a.split('/').cmp(b.split('/')));
    out.into_iter().map(|r| root.join(r)).collect()
}

/// `*.py` matches by file name anywhere; patterns with a slash match the whole relative path and support `**`.
pub fn glob_match(rel: &str, pattern: &str) -> bool {
    if !pattern.contains('/') {
        return fnmatch(rel.rsplit('/').next().unwrap_or(rel), pattern);
    }
    match globset::GlobBuilder::new(pattern).literal_separator(true).build() {
        Ok(g) => g.compile_matcher().is_match(rel),
        Err(_) => false,
    }
}

pub struct SearchResult {
    pub lines: Vec<String>,
    pub total: usize,
    pub hit_files: usize,
    pub per_file: Vec<(String, usize)>,
}

/// Output lines, total matches and files with matches. Stops printing once `limit` matches are collected but keeps
/// counting so the caller can say how much was cut.
pub fn search(files: &[PathBuf], root: &Path, regex: &Re, context: usize, limit: usize, glob: Option<&str>, files_only: bool) -> SearchResult {
    let mut res = SearchResult { lines: vec![], total: 0, hit_files: 0, per_file: vec![] };
    let mut shown = 0usize;
    // one pass over the whole file first: most files have no match and never get split into lines
    // \A and \z mean line start/end per line but file start/end here: no shortcut at all for those
    let whole = if regex.as_str().contains("\\A") || regex.as_str().contains("\\z") { None } else { regex.multiline() };
    let edgy = ["^", "$", "(?"].iter().any(|c| regex.as_str().contains(c));
    let odd_breaks = |t: &str| t.chars().any(|c| matches!(c, '\r' | '\x0b' | '\x0c' | '\x1c'..='\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'));
    for f in files {
        let rel = f.strip_prefix(root).unwrap_or(f).to_string_lossy().replace('\\', "/");
        if let Some(g) = glob {
            if !glob_match(&rel, g) {
                continue;
            }
        }
        let Ok(raw) = std::fs::read(f) else { continue };
        if raw[..raw.len().min(4096)].contains(&0) {
            continue; // binary
        }
        let text = String::from_utf8_lossy(&raw);
        if let Some(w) = &whole {
            if !w.is_match(&text) && !(edgy && odd_breaks(&text)) {
                continue;
            }
        }
        let lines: Vec<&str> = split_lines(&text);
        let hits: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| regex.is_match(l)).map(|(i, _)| i).collect();
        if hits.is_empty() {
            continue;
        }
        res.hit_files += 1;
        res.total += hits.len();
        res.per_file.push((rel.clone(), hits.len())); // (path, matches) for every file, even past the limit
        if files_only {
            res.lines.push(format!("{rel} ({})", hits.len()));
            continue;
        }
        if shown >= limit {
            continue;
        }
        let mut keep = std::collections::BTreeSet::new();
        for &i in &hits {
            if shown >= limit {
                break;
            }
            shown += 1;
            for k in i.saturating_sub(context)..(i + context + 1).min(lines.len()) {
                keep.insert(k);
            }
        }
        let hit_set: std::collections::HashSet<usize> = hits.into_iter().collect();
        let mut prev: i64 = -2;
        for i in keep {
            if context > 0 && i as i64 != prev + 1 && prev >= 0 {
                res.lines.push("--".into());
            }
            let l = lines[i];
            let t = if l.chars().count() <= MAX_LINE_CHARS { l.to_string() } else { format!("{} …", l.chars().take(MAX_LINE_CHARS).collect::<String>()) };
            let sep = if hit_set.contains(&i) { ':' } else { '-' };
            res.lines.push(format!("{rel}{sep}{}{sep}{t}", i + 1));
            prev = i as i64;
        }
    }
    res
}

/// Python's `str.splitlines()`: breaks at \n, \r\n, \r and a few rarer characters, with no empty last line.
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let b = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        let c = text[i..].chars().next().unwrap();
        let brk = matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if brk {
            out.push(&text[start..i]);
            let mut adv = c.len_utf8();
            if c == '\r' && b.get(i + 1) == Some(&b'\n') {
                adv += 1;
            }
            i += adv;
            start = i;
        } else {
            i += c.len_utf8();
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Hint for a guessed path: real files with the same name (or stem) under root.
pub fn similar_paths(root: &Path, missing: &Path, forbidden: &[String]) -> String {
    let files = list_files(root, forbidden);
    let name = missing.file_name();
    let stem = missing.file_stem();
    let mut same: Vec<&PathBuf> = files.iter().filter(|f| f.file_name() == name).collect();
    if same.is_empty() && stem.is_some() {
        same = files.iter().filter(|f| f.file_stem() == stem).collect();
    }
    if same.is_empty() {
        return " Don't guess paths: locate it with glob or grep files_only first.".into();
    }
    let shown: Vec<String> = same.iter().take(5).map(|f| f.strip_prefix(root).unwrap_or(f).to_string_lossy().to_string()).collect();
    format!(" Files with that name: {}", shown.join(", "))
}

/// Notes on paths a sub-agent report names that are not on disk, with the real file of that name.
pub fn check_paths(root: &Path, text: &str, forbidden: &[String]) -> String {
    static P: OnceLock<Re> = OnceLock::new();
    let re = P.get_or_init(|| crate::util::re::lit(r"(?:^|[^\w/.:])(/?(?:[\w.()\[\]@-]+/)+[\w.()\[\]@-]+\.[A-Za-z]{1,5})\b"));
    let mut paths: Vec<String> = vec![];
    let mut at = 0;
    while at < text.len() {
        let Some(c) = re.captures(&text[at..]) else { break };
        let (_, e) = re.find(&text[at..]).unwrap();
        if let Some(p) = c.get(1).cloned().flatten() {
            let abs = if p.starts_with('/') { PathBuf::from(&p) } else { root.join(&p) };
            if !abs.exists() && !paths.contains(&p) {
                paths.push(p);
            }
        }
        at += e.max(1);
    }
    paths.sort();
    if paths.is_empty() {
        return String::new();
    }
    let files = list_files(root, forbidden);
    let rows: Vec<String> = paths
        .iter()
        .take(10)
        .map(|p| {
            let base = Path::new(p).file_name();
            let same: Vec<String> = files.iter().filter(|f| f.file_name() == base).take(3).map(|f| f.strip_prefix(root).unwrap_or(f).to_string_lossy().to_string()).collect();
            format!("- {p}: not on disk{}", if same.is_empty() { "; locate it with glob before reading".to_string() } else { format!("; files with that name: {}", same.join(", ")) })
        })
        .collect();
    format!("Paths in this report that do not exist (checked by the harness; do not read them as written):\n{}", rows.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lines_and_globs() {
        assert_eq!(split_lines("a\r\nb\nc"), vec!["a", "b", "c"]);
        assert_eq!(split_lines("a\n"), vec!["a"]);
        assert!(glob_match("src/a/b.ts", "src/**/*.ts"));
        assert!(glob_match("x/y/z.py", "*.py"));
        assert!(!glob_match("x/y/z.py", "y/*.py"));
    }
}
