//! Read-only access to a graphify code graph (graphify-out/graph.json). Nodes (files, functions, classes, methods) and
//! edges (calls, contains, imports) with file:line. The graph is a snapshot, so callers must treat answers as pointers
//! to verify, not as facts.
use crate::util::paths::resolve;
use crate::util::re::Re;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub fn locate(cwd: &Path) -> Option<PathBuf> {
    let c = resolve(cwd);
    c.ancestors().take(6).map(|d| d.join("graphify-out/graph.json")).find(|f| f.is_file())
}

pub fn norm(label: &str) -> String {
    let l = label.trim().trim_start_matches('.');
    l.strip_suffix("()").unwrap_or(l).to_lowercase()
}

pub fn kind_of(node: &Value) -> &'static str {
    let label = node["label"].as_str().unwrap_or("");
    if label.ends_with("()") {
        return if label.starts_with('.') { "method" } else { "function" };
    }
    if label.contains('.') && !label.contains('/') && !label.starts_with('.') {
        return "file";
    }
    if label.chars().next().map_or(false, |c| c.is_uppercase()) { "class" } else { "symbol" }
}

pub struct CodeGraph {
    pub workspace: PathBuf,
    pub built: std::time::SystemTime,
    pub nodes: BTreeMap<String, Value>,
    pub out: HashMap<String, Vec<(String, Value)>>,
    pub inc: HashMap<String, Vec<(String, Value)>>,
    rel_cache: Mutex<HashMap<String, (String, Option<PathBuf>)>>,
    text_cache: Mutex<HashMap<String, String>>,
}

type Cached = ((PathBuf, std::time::SystemTime), Arc<CodeGraph>);

pub fn load(cwd: &Path) -> Option<Arc<CodeGraph>> {
    static C: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    let path = locate(cwd)?;
    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
    let cache = C.get_or_init(|| Mutex::new(None));
    let mut g = cache.lock().unwrap();
    if let Some((k, v)) = g.as_ref() {
        if *k == (path.clone(), mtime) {
            return Some(v.clone());
        }
    }
    let graph = Arc::new(CodeGraph::new(&path, cwd)?);
    *g = Some(((path, mtime), graph.clone()));
    Some(graph)
}

impl CodeGraph {
    fn new(path: &Path, cwd: &Path) -> Option<CodeGraph> {
        let data: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        let nodes: BTreeMap<String, Value> = data["nodes"].as_array()?.iter().filter_map(|n| n["id"].as_str().map(|i| (i.to_string(), n.clone()))).collect();
        let (mut out, mut inc): (HashMap<String, Vec<(String, Value)>>, HashMap<String, Vec<(String, Value)>>) = (HashMap::new(), HashMap::new());
        for e in data["links"].as_array().into_iter().flatten() {
            let (s, t) = (e["source"].as_str().unwrap_or(""), e["target"].as_str().unwrap_or(""));
            if nodes.contains_key(s) && nodes.contains_key(t) {
                out.entry(s.into()).or_default().push((t.into(), e.clone()));
                inc.entry(t.into()).or_default().push((s.into(), e.clone()));
            }
        }
        Some(CodeGraph {
            workspace: resolve(cwd),
            built: std::fs::metadata(path).ok()?.modified().ok()?,
            nodes,
            out,
            inc,
            rel_cache: Mutex::new(HashMap::new()),
            text_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Graph paths are absolute from wherever it was built; map them onto this workspace by finding the longest suffix
    /// that exists here (relative to the workspace or its parents).
    pub fn rel(&self, source_file: &str) -> String {
        if let Some((r, _)) = self.rel_cache.lock().unwrap().get(source_file) {
            return r.clone();
        }
        let parts: Vec<&str> = Path::new(source_file).components().map(|c| c.as_os_str().to_str().unwrap_or("")).collect();
        let (mut found, mut absolute) = (source_file.to_string(), None);
        for i in 1..parts.len() {
            let cand: PathBuf = parts[i..].iter().collect();
            if let Some(b) = self.workspace.ancestors().take(3).find(|b| b.join(&cand).exists()) {
                found = cand.to_string_lossy().replace('\\', "/");
                absolute = Some(b.join(&cand));
                break;
            }
        }
        self.rel_cache.lock().unwrap().insert(source_file.to_string(), (found.clone(), absolute));
        found
    }

    pub fn where_(&self, nid: &str) -> String {
        let n = &self.nodes[nid];
        let loc = n["source_location"].as_str().map(String::from).unwrap_or_else(|| n["source_location"].to_string().trim_matches('"').replace("null", ""));
        format!("{}:{}", self.rel(n["source_file"].as_str().unwrap_or("")), loc.trim_start_matches('L'))
    }

    pub fn degree(&self, nid: &str) -> usize {
        self.out.get(nid).map_or(0, Vec::len) + self.inc.get(nid).map_or(0, Vec::len)
    }

    pub fn label(&self, nid: &str) -> String {
        self.nodes[nid]["label"].as_str().unwrap_or("").to_string()
    }

    pub fn find(&self, query: &str, limit: usize) -> Vec<String> {
        let q = norm(query);
        let mut scored: Vec<(u8, std::cmp::Reverse<usize>, String)> = vec![];
        for (nid, n) in &self.nodes {
            let label = norm(n["label"].as_str().unwrap_or(""));
            let score = if label == q {
                0
            } else if label.starts_with(&q) {
                1
            } else if label.contains(&q) {
                2
            } else if n["source_file"].as_str().unwrap_or("").to_lowercase().contains(&q) {
                3
            } else {
                continue;
            };
            scored.push((score, std::cmp::Reverse(self.degree(nid)), nid.clone()));
        }
        scored.sort();
        scored.into_iter().take(limit).map(|x| x.2).collect()
    }

    /// Best node for a name, plus other candidates when it is ambiguous.
    pub fn resolve(&self, name: &str) -> (Option<String>, Vec<String>) {
        let hits = self.find(name, 8);
        let exact: Vec<String> = hits.iter().filter(|h| norm(&self.label(h)) == norm(name)).cloned().collect();
        let pool = if exact.is_empty() { hits } else { exact };
        (pool.first().cloned(), pool.into_iter().skip(1).take(4).collect())
    }

    pub fn path(&self, a: &str, b: &str) -> Option<Vec<String>> {
        let mut seen: HashMap<String, Option<String>> = HashMap::from([(a.to_string(), None)]);
        let mut queue = VecDeque::from([a.to_string()]);
        while let Some(cur) = queue.pop_front() {
            if cur == b {
                let mut out = vec![];
                let mut c = Some(cur);
                while let Some(x) = c {
                    c = seen[&x].clone();
                    out.push(x);
                }
                out.reverse();
                return Some(out);
            }
            if seen.len() > 200_000 {
                break;
            }
            for (other, _) in self.out.get(&cur).into_iter().flatten().chain(self.inc.get(&cur).into_iter().flatten()) {
                if !seen.contains_key(other) {
                    seen.insert(other.clone(), Some(cur.clone()));
                    queue.push_back(other.clone());
                }
            }
        }
        None
    }

    /// Files OUTSIDE `files` whose code depends on code inside them: the blast radius of a change.
    pub fn impact(&self, files: &[String], limit: usize) -> Vec<(String, Vec<String>)> {
        let want: HashSet<String> = files.iter().map(|f| f.trim_start_matches("./").to_string()).collect();
        let seeds: HashSet<&String> = self.nodes.iter().filter(|(_, n)| want.contains(&self.rel(n["source_file"].as_str().unwrap_or("")))).map(|(i, _)| i).collect();
        let mut hits: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for nid in &seeds {
            for (other, e) in self.out.get(*nid).into_iter().flatten().chain(self.inc.get(*nid).into_iter().flatten()) {
                let f = self.rel(self.nodes[other]["source_file"].as_str().unwrap_or(""));
                if seeds.contains(other) || want.contains(&f) || f.starts_with('/') || e["relation"] == "contains" {
                    continue; // "/" = a path outside this workspace
                }
                if e["relation"] == "calls" && self.call_direction(nid, other) != "caller" {
                    continue;
                }
                hits.entry(f).or_default().insert(self.label(other));
            }
        }
        let mut ranked: Vec<(String, Vec<String>)> = hits.into_iter().map(|(f, l)| (f, l.into_iter().collect())).collect();
        ranked.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        ranked.into_iter().take(limit).map(|(f, l)| (f, l.into_iter().take(4).collect())).collect()
    }

    pub fn community(&self, cid: &Value) -> Vec<String> {
        let mut v: Vec<String> = self.nodes.iter().filter(|(_, n)| n["community"] == *cid).map(|(i, _)| i.clone()).collect();
        v.sort_by_key(|x| std::cmp::Reverse(self.degree(x)));
        v
    }

    /// A note when commits landed after the graph was built.
    pub fn staleness(&self) -> Option<String> {
        let secs = self.built.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
        let since = chrono::DateTime::from_timestamp(secs, 0)?.with_timezone(&chrono::Local).format("%Y-%m-%dT%H:%M:%S").to_string();
        let o = std::process::Command::new("git").args(["log", "--since", &since, "--oneline"]).current_dir(&self.workspace).output().ok()?;
        let n = String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.trim().is_empty()).count();
        (n > 0).then(|| format!("graph built {}; {n} commit(s) since then, so recent changes may be missing", &since[..10]))
    }

    // ---- direction: the graph's edge orientation is not reliable, the source text is
    fn text(&self, nid: &str) -> String {
        let src = self.nodes[nid]["source_file"].as_str().unwrap_or("").to_string();
        self.rel(&src);
        if let Some(t) = self.text_cache.lock().unwrap().get(&src) {
            return t.clone();
        }
        let abs = self.rel_cache.lock().unwrap().get(&src).and_then(|x| x.1.clone());
        let t = abs.and_then(|a| std::fs::read_to_string(a).ok()).unwrap_or_default();
        self.text_cache.lock().unwrap().insert(src, t.clone());
        t
    }

    /// How many times the code in src's file calls dst by name (definitions excluded).
    fn call_count(&self, src: &str, dst: &str) -> usize {
        let label = self.label(dst);
        let l = label.trim().trim_start_matches('.');
        let l = l.strip_suffix("()").unwrap_or(l);
        let name = regex::escape(l.rsplit('.').next().unwrap_or(l));
        let text = self.text(src);
        let calls = Re::new(&format!(r"\b{name}\s*\("), false).map(|r| count(&r, &text)).unwrap_or(0);
        let defs = Re::new(&format!(r"(?m)^[ \t]*(?:export\s+)?(?:default\s+)?(?:(?:public|private|protected|static|async|readonly|get|set)\s+)*(?:function\*?\s+|def\s+)?{name}\s*\("), false)
            .map(|r| count(&r, &text))
            .unwrap_or(0);
        calls.saturating_sub(defs)
    }

    /// Who calls whom between two call-linked nodes: 'caller' (other calls node), 'callee' (node calls other) or 'unclear'.
    pub fn call_direction(&self, node: &str, other: &str) -> &'static str {
        let (other_calls_node, node_calls_other) = (self.call_count(other, node) > 0, self.call_count(node, other) > 0);
        match (other_calls_node, node_calls_other) {
            (true, false) => "caller",
            (false, true) => "callee",
            _ => "unclear",
        }
    }
}

fn count(re: &Re, text: &str) -> usize {
    let (mut n, mut at) = (0, 0);
    while at <= text.len() {
        let Some((_, e)) = re.find(&text[at..]) else { break };
        n += 1;
        at += e.max(1);
        while at < text.len() && !text.is_char_boundary(at) {
            at += 1;
        }
    }
    n
}
