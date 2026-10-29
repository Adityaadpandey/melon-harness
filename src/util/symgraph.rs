//! Melon's own symbol graph: what is defined where, and who calls whom, read straight from the working tree with
//! tree-sitter. No snapshot to go stale and no external tool. Each file is parsed once and re-parsed only when its
//! size or mtime changes, so asking again after an edit costs one file, not the repo.
//!
//! Calls are matched by name, not by type. `foo()` in a.ts links to every `foo` it could mean: one in the same file, one
//! in a file a.ts imports, or (for languages where we don't track imports yet) any `foo` in the repo. That is a pointer to
//! verify in the source, never a proof.
use crate::util::importgraph;
use crate::util::paths::resolve;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use tree_sitter::{Language, Node, Parser};

const EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs", "go"];
const MAX_FILES: usize = 60_000;
const MAX_BYTES: u64 = 1_000_000;

#[derive(Clone, Debug)]
pub struct Def {
    pub name: String,
    pub kind: &'static str,
    pub line: usize,
    pub end: usize,
    /// The class / impl / trait it sits in, e.g. `Router` for `Router.handle`.
    pub parent: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Call {
    /// Index into the file's defs, or None for code at the top level of the file.
    pub from: Option<usize>,
    pub name: String,
    pub line: usize,
}

#[derive(Default, Debug)]
pub struct FileSyms {
    pub defs: Vec<Def>,
    pub calls: Vec<Call>,
}

fn language(ext: &str) -> Option<Language> {
    Some(match ext {
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        _ => return None,
    })
}

fn text<'a>(n: Node, src: &'a str) -> &'a str {
    src.get(n.byte_range()).unwrap_or("")
}

/// The name a definition node introduces, plus what kind of thing it is.
fn def_of(n: Node, src: &str) -> Option<(String, &'static str)> {
    let kind = match n.kind() {
        "function_declaration" | "generator_function_declaration" | "function_definition" | "function_item" => "function",
        "method_definition" | "method_declaration" => "method",
        "class_declaration" | "abstract_class_declaration" | "class_definition" => "class",
        "interface_declaration" => "interface",
        "type_alias_declaration" | "type_spec" => "type",
        "enum_declaration" | "enum_item" => "enum",
        "struct_item" => "struct",
        "trait_item" => "trait",
        "mod_item" => "mod",
        "impl_item" => "impl",
        // const run = () => {...}  counts as the function `run`
        "variable_declarator" => {
            let v = n.child_by_field_name("value")?;
            if !matches!(v.kind(), "arrow_function" | "function_expression" | "function") {
                return None;
            }
            "function"
        }
        _ => return None,
    };
    let name_node = if kind == "impl" { n.child_by_field_name("type")? } else { n.child_by_field_name("name")? };
    let raw = text(name_node, src);
    // impl<T> Foo<T>  ->  Foo
    let name = raw.split(['<', ' ']).next().unwrap_or(raw).trim();
    (!name.is_empty()).then(|| (name.to_string(), kind))
}

/// `a.b.c(x)` -> "c", `Foo::new(x)` -> "new", `run(x)` -> "run".
fn callee(n: Node, src: &str) -> Option<String> {
    let field = match n.kind() {
        "identifier" | "property_identifier" | "field_identifier" | "type_identifier" => return Some(text(n, src).to_string()),
        "member_expression" => "property",
        "attribute" => "attribute",
        "field_expression" | "selector_expression" => "field",
        "scoped_identifier" => "name",
        "generic_function" => "function",
        _ => return None,
    };
    callee(n.child_by_field_name(field)?, src)
}

fn walk(n: Node, src: &str, cur: Option<usize>, parent: &Option<String>, out: &mut FileSyms, depth: usize) {
    if depth > 600 {
        return; // pathological nesting (minified bundles); the stack matters more than the last 0.1%
    }
    let (mut cur, mut parent_here) = (cur, parent.clone());
    if let Some((name, mut kind)) = def_of(n, src) {
        if kind == "function" && parent.is_some() {
            kind = "method";
        }
        out.defs.push(Def { name: name.clone(), kind, line: n.start_position().row + 1, end: n.end_position().row + 1, parent: parent.clone() });
        cur = Some(out.defs.len() - 1);
        if matches!(kind, "class" | "impl" | "trait" | "interface" | "mod") {
            parent_here = Some(name);
        }
    }
    let target = match n.kind() {
        "call_expression" | "call" => n.child_by_field_name("function"),
        "new_expression" => n.child_by_field_name("constructor"),
        _ => None,
    };
    if let Some(name) = target.and_then(|t| callee(t, src)) {
        out.calls.push(Call { from: cur, name, line: n.start_position().row + 1 });
    }
    let mut c = n.walk();
    for child in n.children(&mut c) {
        walk(child, src, cur, &parent_here, out, depth + 1);
    }
}

pub fn extract(path: &str, src: &str) -> Option<FileSyms> {
    let ext = path.rsplit_once('.')?.1.to_lowercase();
    let mut p = Parser::new();
    p.set_language(&language(&ext)?).ok()?;
    let tree = p.parse(src, None)?;
    let mut out = FileSyms::default();
    walk(tree.root_node(), src, None, &None, &mut out, 0);
    Some(out)
}

type Stamp = (u64, u128);
type Entry = (Stamp, Arc<FileSyms>);

pub struct SymGraph {
    pub files: HashMap<String, Arc<FileSyms>>,
    /// name -> every (file, def index) with that name
    by_name: HashMap<String, Vec<(String, usize)>>,
    /// callee name -> every (file, call index) that calls it
    called: HashMap<String, Vec<(String, usize)>>,
    imports: Arc<importgraph::ImportGraph>,
}

#[derive(Debug, PartialEq)]
pub struct Ref {
    pub file: String,
    /// The def the call sits in, `None` for top-level code.
    pub in_def: Option<String>,
    pub line: usize,
}

impl SymGraph {
    fn assemble(files: HashMap<String, Arc<FileSyms>>, imports: Arc<importgraph::ImportGraph>) -> SymGraph {
        let (mut by_name, mut called): (HashMap<String, Vec<(String, usize)>>, HashMap<String, Vec<(String, usize)>>) = Default::default();
        for (f, s) in &files {
            for (i, d) in s.defs.iter().enumerate() {
                by_name.entry(d.name.clone()).or_default().push((f.clone(), i));
            }
            for (i, c) in s.calls.iter().enumerate() {
                called.entry(c.name.clone()).or_default().push((f.clone(), i));
            }
        }
        SymGraph { files, by_name, called, imports }
    }

    /// Could a call to `name` made from `from_file` mean the definition in `def_file`?
    fn reachable(&self, from_file: &str, def_file: &str) -> bool {
        if from_file == def_file {
            return true;
        }
        // Imports are only tracked for py/js/ts. For the rest, any file may mean it.
        match self.imports.out.get(from_file) {
            Some(out) if self.imports.out.contains_key(def_file) => out.contains(def_file),
            _ => true,
        }
    }

    /// Definitions matching `query`: exact name first, then prefix, then substring. Case-insensitive.
    pub fn find(&self, query: &str, limit: usize) -> Vec<(String, Def)> {
        let q = query.trim().trim_end_matches("()").to_lowercase();
        let mut hits: Vec<(u8, String, Def)> = vec![];
        for (name, at) in &self.by_name {
            let n = name.to_lowercase();
            let rank = if n == q { 0 } else if n.starts_with(&q) { 1 } else if n.contains(&q) { 2 } else { continue };
            for (f, i) in at {
                hits.push((rank, f.clone(), self.files[f].defs[*i].clone()));
            }
        }
        hits.sort_by(|a, b| (a.0, &a.1, a.2.line).cmp(&(b.0, &b.1, b.2.line)));
        hits.into_iter().take(limit).map(|(_, f, d)| (f, d)).collect()
    }

    /// Places that call the definition `name` in `file`.
    pub fn callers(&self, file: &str, name: &str) -> Vec<Ref> {
        let mut out: Vec<Ref> = self
            .called
            .get(name)
            .into_iter()
            .flatten()
            .filter(|(f, _)| self.reachable(f, file))
            .map(|(f, i)| {
                let s = &self.files[f];
                let c = &s.calls[*i];
                Ref { file: f.clone(), in_def: c.from.map(|d| s.defs[d].name.clone()), line: c.line }
            })
            .collect();
        out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        out
    }

    /// Definitions that the body of `name` in `file` calls (the ones we can find in the repo).
    pub fn callees(&self, file: &str, name: &str) -> Vec<(String, Def)> {
        let Some(s) = self.files.get(file) else { return vec![] };
        let mut seen = std::collections::BTreeSet::new();
        let mut out = vec![];
        for (di, d) in s.defs.iter().enumerate().filter(|(_, d)| d.name == name) {
            let _ = d;
            for c in s.calls.iter().filter(|c| c.from == Some(di)) {
                for (f, i) in self.by_name.get(&c.name).into_iter().flatten() {
                    if self.reachable(file, f) && seen.insert((f.clone(), *i)) {
                        out.push((f.clone(), self.files[f].defs[*i].clone()));
                    }
                }
            }
        }
        out
    }
}

fn listed(cwd: &Path) -> Vec<String> {
    let Ok(o) = std::process::Command::new("git").args(["ls-files", "--cached", "--others", "--exclude-standard"]).current_dir(cwd).output() else { return vec![] };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|f| Path::new(f).extension().map_or(false, |e| EXTS.contains(&e.to_string_lossy().to_lowercase().as_str())))
        .take(MAX_FILES)
        .map(String::from)
        .collect()
}

fn stamp(p: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(p).ok()?;
    if m.len() > MAX_BYTES {
        return None;
    }
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((m.len(), t))
}

/// Parse `todo` across all cores. A parser is cheap but not shareable, so each thread owns one.
fn parse_all(cwd: &Path, todo: Vec<(String, Stamp)>) -> Vec<(String, Entry)> {
    if todo.is_empty() {
        return vec![];
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(todo.len());
    let chunk = todo.len().div_ceil(threads);
    let mut done = vec![];
    std::thread::scope(|sc| {
        let handles: Vec<_> = todo
            .chunks(chunk)
            .map(|part| {
                // big stack: the tree walk is recursive
                std::thread::Builder::new()
                    .stack_size(32 << 20)
                    .spawn_scoped(sc, move || {
                        part.iter()
                            .map(|(f, st)| {
                                let syms = std::fs::read_to_string(cwd.join(f)).ok().and_then(|s| extract(f, &s)).unwrap_or_default();
                                (f.clone(), (*st, Arc::new(syms)))
                            })
                            .collect::<Vec<_>>()
                    })
                    .expect("spawn parse thread")
            })
            .collect();
        for h in handles {
            done.extend(h.join().unwrap_or_default());
        }
    });
    done
}

type Cache = HashMap<String, (HashMap<String, Entry>, Option<(u64, Arc<SymGraph>)>)>;

/// The graph for this working tree. Unchanged files come from the cache; only new or edited ones are parsed.
pub fn load(cwd: &Path) -> Arc<SymGraph> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cwd = resolve(cwd);
    let key = cwd.to_string_lossy().to_string();
    let cache = C.get_or_init(|| Mutex::new(HashMap::new()));
    let (mut parsed, last) = cache.lock().unwrap().remove(&key).unwrap_or_default();

    let mut live: Vec<(String, Stamp)> = listed(&cwd).into_iter().filter_map(|f| stamp(&cwd.join(&f)).map(|s| (f, s))).collect();
    live.sort();
    let todo: Vec<(String, Stamp)> = live.iter().filter(|(f, s)| parsed.get(f).map_or(true, |(old, _)| old != s)).cloned().collect();
    let fingerprint = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        live.hash(&mut h);
        h.finish()
    };
    if todo.is_empty() {
        if let Some((fp, g)) = last.as_ref() {
            if *fp == fingerprint {
                let g = g.clone();
                cache.lock().unwrap().insert(key, (parsed, last));
                return g;
            }
        }
    }
    parsed.extend(parse_all(&cwd, todo));
    let names: std::collections::HashSet<&String> = live.iter().map(|(f, _)| f).collect();
    parsed.retain(|f, _| names.contains(f)); // deleted files drop out
    let files = parsed.iter().map(|(f, (_, s))| (f.clone(), s.clone())).collect();
    let g = Arc::new(SymGraph::assemble(files, importgraph::load(&cwd)));
    cache.lock().unwrap().insert(key, (parsed, Some((fingerprint, g.clone()))));
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &FileSyms) -> Vec<String> {
        s.defs.iter().map(|d| format!("{}:{}", d.kind, d.name)).collect()
    }

    #[test]
    fn typescript_defs_and_calls() {
        let s = extract("a.ts", "export function load() { return parse(x); }\nconst run = () => { load(); api.send(1); };\nclass Router { handle() { load(); } }").unwrap();
        assert_eq!(names(&s), ["function:load", "function:run", "class:Router", "method:handle"]);
        let by: Vec<(String, Option<usize>)> = s.calls.iter().map(|c| (c.name.clone(), c.from)).collect();
        assert_eq!(by, [("parse".into(), Some(0)), ("load".into(), Some(1)), ("send".into(), Some(1)), ("load".into(), Some(3))]);
        assert_eq!(s.defs[3].parent.as_deref(), Some("Router"));
    }

    #[test]
    fn python_rust_go() {
        let py = extract("a.py", "class A:\n    def go(self):\n        helper()\n").unwrap();
        assert_eq!(names(&py), ["class:A", "method:go"]);
        assert_eq!(py.calls[0].name, "helper");
        let rs = extract("a.rs", "struct S;\nimpl S { fn new() -> S { make(); S } }\nfn make() {}").unwrap();
        assert_eq!(names(&rs), ["struct:S", "impl:S", "method:new", "function:make"]);
        assert_eq!(rs.calls[0].name, "make");
        let go = extract("a.go", "package a\nfunc (r *R) Do() { fmt.Println(helper()) }\nfunc helper() int { return 1 }").unwrap();
        assert_eq!(names(&go), ["method:Do", "function:helper"]);
        assert!(go.calls.iter().any(|c| c.name == "helper") && go.calls.iter().any(|c| c.name == "Println"));
    }

    #[test]
    fn graph_links_callers_and_callees_and_picks_up_edits() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::process::Command::new("git").args(["init", "-q"]).current_dir(p).status().unwrap();
        std::fs::write(p.join("lib.py"), "def parse(x):\n    return x\n").unwrap();
        std::fs::write(p.join("app.py"), "from lib import parse\n\ndef main():\n    parse(1)\n").unwrap();
        std::fs::write(p.join("other.py"), "def parse(x):\n    pass\n\ndef side():\n    parse(2)\n").unwrap();
        let g = load(p);
        let callers = g.callers("lib.py", "parse");
        // other.py has its own parse and doesn't import lib, so its call is not ours
        assert_eq!(callers, [Ref { file: "app.py".into(), in_def: Some("main".into()), line: 4 }]);
        assert_eq!(g.callees("app.py", "main")[0].0, "lib.py");
        assert_eq!(g.find("pars", 10).len(), 2);

        std::fs::write(p.join("app.py"), "from lib import parse\n\ndef main():\n    pass\n\ndef later():\n    parse(3)\n").unwrap();
        let callers = load(p).callers("lib.py", "parse");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].in_def.as_deref(), Some("later"));
    }
}
