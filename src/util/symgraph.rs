//! Melon's own symbol graph: what is defined where, and who calls whom, read straight from the working tree with
//! tree-sitter. No snapshot to go stale and no external tool. Each file is parsed once and re-parsed only when its
//! size or mtime changes, so asking again after an edit costs one file, not the repo.
//!
//! Calls are matched by name, not by type. `foo()` in a.ts links to every `foo` it could mean: one in the same file, one
//! in a file a.ts imports, or (for languages where we don't track imports yet) any `foo` in the repo. That is a pointer to
//! verify in the source, never a proof.
use crate::util::importgraph;
use crate::util::paths::resolve;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use tree_sitter::{Language, Node, Parser};

const EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs", "go", "java", "cs", "php", "rb"];
const MAX_FILES: usize = 60_000;
const MAX_BYTES: u64 = 1_000_000;

#[derive(Clone, Debug, Serialize)]
pub struct Def {
    pub name: String,
    pub kind: &'static str,
    pub line: usize,
    pub end: usize,
    /// The class / impl / trait it sits in, e.g. `Router` for `Router.handle`.
    pub parent: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Call {
    /// Index into the file's defs, or None for code at the top level of the file.
    pub from: Option<usize>,
    pub name: String,
    pub line: usize,
    /// What the name was written after, which is how a call is told apart from its namesakes:
    /// `symgraph::load()` -> "symgraph", `Config::new()` -> "Config", `cfg.load()` -> ".cfg", `a.b().load()` -> ".",
    /// plain `load()` or a type mentioned by name -> None.
    #[serde(default)]
    pub qual: Option<String>,
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct FileSyms {
    pub defs: Vec<Def>,
    pub calls: Vec<Call>,
    /// Rust `use` lines, name -> the module it came from: `use crate::util::importgraph::load;` -> load: importgraph.
    /// A later plain `load()` in that file then means importgraph's, not every `load` in the repo.
    #[serde(default)]
    pub uses: HashMap<String, String>,
}

/// Method names the standard library answers for almost every value. `x.len()` on an unknown receiver says nothing about
/// a `len` this repo defines, so no link is claimed. ponytail: a fixed list; real receiver types would need a type checker.
const EVERYWHERE: &[&str] = &[
    "len", "is_empty", "push", "pop", "insert", "remove", "get", "set", "clone", "iter", "map", "filter", "find", "contains", "extend", "join", "next", "take", "clear", "first", "last", "keys", "values", "sort",
    "to_string", "into", "from", "default", "fmt", "eq", "cmp", "hash", "as_str", "parse", "write", "read", "flush", "lock", "send", "recv", "trim", "split", "replace", "starts_with", "ends_with", "append", "unwrap",
];

/// Names a Rust `use` brings in, each with the last module on its path: `use a::b::{c, d::e, f as g};` -> c: b, e: d, g: b.
fn rust_uses(n: Node, src: &str, module: &str, out: &mut HashMap<String, String>) {
    let leaf = |t: &str| t.rsplit("::").next().unwrap_or(t).to_string();
    match n.kind() {
        "identifier" | "type_identifier" if !module.is_empty() => {
            out.insert(text(n, src).to_string(), module.to_string());
        }
        "scoped_identifier" => {
            if let (Some(p), Some(name)) = (n.child_by_field_name("path"), n.child_by_field_name("name")) {
                out.insert(text(name, src).to_string(), leaf(text(p, src)));
            }
        }
        "use_as_clause" => {
            if let (Some(p), Some(alias)) = (n.child_by_field_name("path"), n.child_by_field_name("alias")) {
                let from = if p.kind() == "scoped_identifier" { p.child_by_field_name("path").map_or(module.to_string(), |x| leaf(text(x, src))) } else { module.to_string() };
                out.insert(text(alias, src).to_string(), from);
            }
        }
        "scoped_use_list" => {
            let m = n.child_by_field_name("path").map_or(module.to_string(), |p| leaf(text(p, src)));
            if let Some(list) = n.child_by_field_name("list") {
                let mut c = list.walk();
                for child in list.children(&mut c) {
                    rust_uses(child, src, &m, out);
                }
            }
        }
        _ => {
            let mut c = n.walk();
            for child in n.children(&mut c) {
                rust_uses(child, src, module, out);
            }
        }
    }
}

const KINDS: &[&str] = &["function", "method", "class", "interface", "type", "enum", "struct", "trait", "mod", "impl"];

/// Kinds are static strings in memory; the saved file holds plain text, mapped back here (an unknown one reads as "symbol").
#[derive(Deserialize)]
struct DefRaw {
    name: String,
    kind: String,
    line: usize,
    end: usize,
    parent: Option<String>,
}

impl<'de> Deserialize<'de> for Def {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Def, D::Error> {
        DefRaw::deserialize(d).map(Def::from)
    }
}

impl From<DefRaw> for Def {
    fn from(r: DefRaw) -> Def {
        Def { kind: KINDS.iter().find(|k| **k == r.kind).copied().unwrap_or("symbol"), name: r.name, line: r.line, end: r.end, parent: r.parent }
    }
}

fn language(ext: &str) -> Option<Language> {
    Some(match ext {
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "cs" => tree_sitter_c_sharp::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "rb" => tree_sitter_ruby::LANGUAGE.into(),
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
        "method_definition" | "method_declaration" | "constructor_declaration" | "singleton_method" => "method",
        "method" => "function", // ruby's def; becomes a method below when it sits in a class
        "class_declaration" | "abstract_class_declaration" | "class_definition" | "record_declaration" | "class" => "class",
        "struct_declaration" => "struct",
        "trait_declaration" => "trait",
        "module" => "mod",
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

/// The called name and what it was written after: `a.b.c(x)` -> ("c", "."), `cfg.load()` -> ("load", ".cfg"),
/// `Foo::new(x)` -> ("new", "Foo"), `run(x)` -> ("run", None).
fn callee(n: Node, src: &str) -> Option<(String, Option<String>)> {
    let (field, on) = match n.kind() {
        "identifier" | "property_identifier" | "field_identifier" | "type_identifier" | "name" | "constant" => return Some((text(n, src).to_string(), None)),
        // PHP \App\Billing\charge() and C# List<int>: the last plain name
        "qualified_name" | "generic_name" | "generic_type" => return Some((text(n, src).split(['<', '(']).next().unwrap_or("").rsplit(['\\', '.']).next().unwrap_or("").trim().to_string(), None)),
        "member_access_expression" => ("name", "expression"),
        "member_expression" => ("property", "object"),
        "attribute" => ("attribute", "object"),
        "field_expression" => ("field", "value"),
        "selector_expression" => ("field", "operand"),
        "scoped_identifier" => ("name", "path"),
        "generic_function" => return callee(n.child_by_field_name("function")?, src),
        _ => return None,
    };
    let (name, _) = callee(n.child_by_field_name(field)?, src)?;
    let owner = n.child_by_field_name(on);
    let qual = if n.kind() == "scoped_identifier" {
        // crate::util::symgraph::load -> symgraph
        owner.map(|o| text(o, src).rsplit("::").next().unwrap_or("").split('<').next().unwrap_or("").to_string())
    } else {
        // a plain receiver keeps its name (self, this, a Go package); anything longer is just "some value"
        Some(match owner {
            Some(o) if matches!(o.kind(), "identifier" | "self" | "this") => format!(".{}", text(o, src)),
            _ => ".".to_string(),
        })
    };
    Some((name, qual.filter(|q| !q.is_empty())))
}

/// `refs` is on for JS/TS/Python, where a function handed over by name (`router.get("/x", handler)`, `onClick={save}`)
/// is how most of them get run, so it counts as a use.
fn walk(n: Node, src: &str, cur: Option<usize>, parent: &Option<String>, out: &mut FileSyms, depth: usize, refs: bool) {
    if depth > 600 {
        return; // pathological nesting (minified bundles); the stack matters more than the last 0.1%
    }
    if n.kind() == "use_declaration" {
        return rust_uses(n, src, "", &mut out.uses);
    }
    let (mut cur, mut parent_here) = (cur, parent.clone());
    if let Some((name, mut kind)) = def_of(n, src) {
        if kind == "function" && parent.is_some() {
            kind = "method";
        }
        // Go writes the owner in front: func (r *Router) Handle() -> parent Router
        let owner = n.child_by_field_name("receiver").and_then(|r| text(r, src).split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).last().map(String::from));
        out.defs.push(Def { name: name.clone(), kind, line: n.start_position().row + 1, end: n.end_position().row + 1, parent: owner.or_else(|| parent.clone()) });
        cur = Some(out.defs.len() - 1);
        if matches!(kind, "class" | "impl" | "trait" | "interface" | "mod") {
            parent_here = Some(name);
        }
    }
    let target = match n.kind() {
        "call_expression" | "call" | "invocation_expression" | "function_call_expression" => n.child_by_field_name("function"),
        "new_expression" => n.child_by_field_name("constructor"),
        "object_creation_expression" => n.child_by_field_name("type").or_else(|| n.named_child(0)),
        _ => None,
    };
    let line = n.start_position().row + 1;
    // Java, PHP and Ruby put the name on the call itself: obj.run(), $obj->run(), Foo::run()
    let named = || {
        let field = match n.kind() {
            "method_invocation" | "member_call_expression" | "scoped_call_expression" => "name",
            "call" => "method",
            _ => return None,
        };
        let name = text(n.child_by_field_name(field)?, src).to_string();
        let owner = ["object", "receiver", "scope"].iter().find_map(|f| n.child_by_field_name(f));
        let qual = owner.map(|o| match o.kind() {
            _ if n.kind() == "scoped_call_expression" => text(o, src).to_string(),
            "identifier" | "constant" | "self" | "this" | "variable_name" => format!(".{}", text(o, src).trim_start_matches('$')),
            _ => ".".to_string(),
        });
        Some((name, qual))
    };
    if let Some((name, qual)) = target.and_then(|t| callee(t, src)).or_else(named) {
        // Config::new() is also a use of Config
        if let Some(ty) = qual.as_ref().filter(|q| q.starts_with(char::is_uppercase) && q.as_str() != "Self") {
            out.calls.push(Call { from: cur, name: ty.clone(), line, qual: None });
        }
        out.calls.push(Call { from: cur, name, line, qual });
    } else {
        let up = n.parent();
        let named_here = |field: &str| up.and_then(|p| p.child_by_field_name(field)).map_or(false, |x| x.id() == n.id());
        let used = match n.kind() {
            // <SiteChrome> renders it, which is the only way a component is ever "called"
            "jsx_opening_element" | "jsx_self_closing_element" => n.child_by_field_name("name").and_then(|t| callee(t, src)).map(|(name, _)| name).filter(|name| name.starts_with(char::is_uppercase)),
            // a type written anywhere but its own definition: fn f(c: Config), Config { .. }, x as Config
            "type_identifier" if !named_here("name") && !(named_here("type") && up.map_or(false, |p| p.kind() == "impl_item")) => Some(text(n, src).to_string()),
            // handed over by name, or the class this one extends
            "identifier" if refs && up.map_or(false, |p| matches!(p.kind(), "arguments" | "argument_list" | "jsx_expression" | "extends_clause" | "class_heritage")) => Some(text(n, src).to_string()),
            _ => None,
        };
        if let Some(name) = used {
            out.calls.push(Call { from: cur, name, line, qual: None });
        }
    }
    let mut c = n.walk();
    for child in n.children(&mut c) {
        walk(child, src, cur, &parent_here, out, depth + 1, refs);
    }
}

pub fn extract(path: &str, src: &str) -> Option<FileSyms> {
    let ext = path.rsplit_once('.')?.1.to_lowercase();
    let mut p = Parser::new();
    p.set_language(&language(&ext)?).ok()?;
    let tree = p.parse(src, None)?;
    let mut out = FileSyms::default();
    walk(tree.root_node(), src, None, &None, &mut out, 0, !matches!(ext.as_str(), "rs" | "go"));
    out.calls.retain(|c| !c.name.is_empty());
    Some(out)
}

type Stamp = (u64, u64);
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
            // directly, or through a barrel: import { x } from "./lib" where lib/index.ts re-exports from lib/x.ts
            Some(out) if self.imports.out.contains_key(def_file) => {
                let barrel = |m: &String| m.rsplit('/').next().map_or(false, |n| n.starts_with("index.") || n == "__init__.py");
                out.contains(def_file) || out.iter().any(|m| barrel(m) && self.imports.out.get(m).map_or(false, |o| o.contains(def_file)))
            }
            _ => true,
        }
    }

    /// Does this call mean that definition? With tracked imports (py/js/ts) the import decides. Rust and Go have no
    /// import graph here, so the way the call is written decides instead:
    ///   symgraph::load()  only the `load` in symgraph.rs (or symgraph/mod.rs), or a method of a type named symgraph
    ///   Config::new()     only the `new` inside impl Config
    ///   self.save()       a method in this file if there is one
    ///   load()            a free function, the one in this file when this file has one
    ///   x.load()          some method; claimed only when few methods share the name, else it would be a guess
    fn means(&self, from_file: &str, call: &Call, def_file: &str, def: &Def) -> bool {
        if !self.reachable(from_file, def_file) {
            return false;
        }
        if self.imports.out.contains_key(from_file) && self.imports.out.contains_key(def_file) {
            return true;
        }
        let local = |method: bool| self.files.get(from_file).map_or(false, |s| s.defs.iter().any(|d| d.name == call.name && d.parent.is_some() == method && !matches!(d.kind, "impl" | "mod")));
        let home = |q: &str| {
            let mut parts = def_file.rsplit('/');
            let stem = parts.next().unwrap_or("").rsplit_once('.').map_or("", |x| x.0);
            stem == q || (parts.next() == Some(q) && (matches!(stem, "mod" | "lib" | "main") || def_file.ends_with(".go")))
        };
        let fits = |q: &str, d: &Def, f: &str| -> bool {
            let mut parts = f.rsplit('/');
            let stem = parts.next().unwrap_or("").rsplit_once('.').map_or("", |x| x.0);
            d.parent.as_deref() == Some(q) || (d.parent.is_none() && (stem == q || (parts.next() == Some(q) && matches!(stem, "mod" | "lib" | "main"))))
        };
        // a plain name the file brought in with `use`: follow the use, unless it points at a re-export we can't see through
        let imported = self.files.get(from_file).and_then(|s| s.uses.get(&call.name)).filter(|m| call.qual.is_none() && self.by_name.get(&call.name).into_iter().flatten().any(|(f, i)| fits(m, &self.files[f].defs[*i], f)));
        if let Some(m) = imported {
            return fits(m, def, def_file);
        }
        match call.qual.as_deref() {
            // a bare name in the same file may also be a sibling method (Java, C#, PHP, Ruby call those without `this`)
            None => from_file == def_file || (def.parent.is_none() && !local(false)),
            Some("Self" | ".self" | ".this") => def.parent.is_some() && (from_file == def_file || !local(true)),
            Some(q) if q.starts_with('.') => {
                let pkg = q.len() > 1 && home(&q[1..]);
                let methods = self.by_name.get(&call.name).into_iter().flatten().filter(|(f, i)| self.files[f].defs[*i].parent.is_some()).count();
                pkg || (def.parent.is_some() && methods <= 3 && !EVERYWHERE.contains(&call.name.as_str()))
            }
            Some("crate" | "super") => def.parent.is_none(),
            Some(q) => fits(q, def, def_file),
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
        let targets: Vec<&Def> = self.files.get(file).map(|s| s.defs.iter().filter(|d| d.name == name).collect()).unwrap_or_default();
        let mut out: Vec<Ref> = self
            .called
            .get(name)
            .into_iter()
            .flatten()
            .filter(|(f, i)| targets.iter().any(|d| self.means(f, &self.files[f].calls[*i], file, d)))
            .map(|(f, i)| {
                let s = &self.files[f];
                let c = &s.calls[*i];
                Ref { file: f.clone(), in_def: c.from.map(|d| s.defs[d].name.clone()), line: c.line }
            })
            .collect();
        out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
        out.dedup(); // Config::new() counts once on its line, not as a call and a type use
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
                    if self.means(file, c, f, &self.files[f].defs[*i]) && seen.insert((f.clone(), *i)) {
                        out.push((f.clone(), self.files[f].defs[*i].clone()));
                    }
                }
            }
        }
        out
    }
}

const MAP_ROWS: usize = 8;

/// One row of a walk through the graph: how many hops from the start, then where.
pub type Hop = (usize, String, String, usize);

impl SymGraph {
    /// Follow the links several hops in one go, as an indented tree in visiting order.
    ///   up   = who calls it, and who calls those ("what breaks if I change this")
    ///   down = what it calls, and what those call ("how does this actually work")
    /// A place already shown is not expanded twice, and the walk stops at `limit` rows so a hub like `log` stays readable.
    pub fn trace(&self, file: &str, name: &str, up: bool, depth: usize, limit: usize) -> Vec<Hop> {
        let mut out: Vec<Hop> = vec![];
        let mut seen: std::collections::HashSet<(String, String)> = [(file.to_string(), name.to_string())].into();
        let mut stack: Vec<(usize, String, String)> = vec![(0, file.to_string(), name.to_string())];
        while let Some((d, f, n)) = stack.pop() {
            if d >= depth || out.len() >= limit {
                continue;
            }
            let next: Vec<(String, String, usize)> = if up {
                self.callers(&f, &n).into_iter().map(|r| (r.file, r.in_def.unwrap_or_else(|| "(top level)".into()), r.line)).collect()
            } else {
                self.callees(&f, &n).into_iter().filter(|(_, c)| matches!(c.kind, "function" | "method")).map(|(cf, c)| (cf, c.name, c.line)).collect()
            };
            let at = out.iter().position(|h| h.0 == d && h.1 == f && h.2 == n).map_or(out.len(), |i| i + 1);
            let mut fresh = vec![];
            for (nf, nn, line) in next {
                if seen.insert((nf.clone(), nn.clone())) {
                    fresh.push((d + 1, nf, nn, line));
                }
            }
            // children go right under their parent, and are expanded next
            for h in fresh.iter().rev() {
                if h.2 != "(top level)" {
                    stack.push((h.0, h.1.clone(), h.2.clone()));
                }
            }
            let room = limit.saturating_sub(out.len());
            out.splice(at..at, fresh.into_iter().take(room));
        }
        out
    }

    /// The shortest chain of calls from one function down to another, e.g. handler -> service -> repo.save.
    pub fn path(&self, file: &str, name: &str, to: &str) -> Option<Vec<(String, String, usize)>> {
        let start = (file.to_string(), name.to_string());
        let mut from: HashMap<(String, String), Option<(String, String)>> = HashMap::from([(start.clone(), None)]);
        let mut lines: HashMap<(String, String), usize> = HashMap::new();
        let mut queue = std::collections::VecDeque::from([start]);
        while let Some(cur) = queue.pop_front() {
            if cur.1 == to && from[&cur].is_some() {
                let mut chain = vec![];
                let mut at = Some(cur);
                while let Some(x) = at {
                    at = from[&x].clone();
                    chain.push((x.0.clone(), x.1.clone(), lines.get(&x).copied().unwrap_or(0)));
                }
                chain.reverse();
                return Some(chain);
            }
            if from.len() > 5000 {
                break;
            }
            for (f, d) in self.callees(&cur.0, &cur.1) {
                let key = (f, d.name);
                if !from.contains_key(&key) {
                    from.insert(key.clone(), Some(cur.clone()));
                    lines.insert(key.clone(), d.line);
                    queue.push_back(key);
                }
            }
        }
        None
    }
}

/// What an edit should say about the rest of the repo. When a function's first line changed (a parameter added, a
/// rename) or the function is gone, the places that use it are listed right in the edit result, so the next step is
/// "update these three callers" instead of a grep to find out whether there are any:
///   [graph: the signature of `parse_invoice` changed and it is used in 2 other place(s): app.py:4 (main), jobs.py:9 (nightly). ...]
/// ponytail: compares the first line of each definition only, so a change on a later line of a multi-line signature is missed.
pub fn edit_note(cwd: &Path, path: &Path, old: &str, new: &str) -> Option<String> {
    let root = resolve(cwd);
    let rel = resolve(path).strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
    let (before, after) = (extract(&rel, old)?, extract(&rel, new)?);
    let head = |src: &str, d: &Def| src.lines().nth(d.line - 1).unwrap_or("").trim().to_string();
    let now: HashMap<(&str, Option<&str>), String> = after.defs.iter().map(|d| ((d.name.as_str(), d.parent.as_deref()), head(new, d))).collect();
    let mut changed: Vec<(&str, &str)> = vec![];
    for d in before.defs.iter().filter(|d| !matches!(d.kind, "impl" | "mod")) {
        match now.get(&(d.name.as_str(), d.parent.as_deref())) {
            None if !after.defs.iter().any(|x| x.name == d.name) => changed.push((&d.name, "was removed or renamed")),
            Some(h) if *h != head(old, d) => changed.push((&d.name, "changed")),
            _ => {}
        }
    }
    if changed.is_empty() {
        return None;
    }
    let g = load(cwd);
    let mut rows = vec![];
    for (name, what) in changed.into_iter().take(3) {
        let mut uses = g.callers(&rel, name);
        if what != "changed" {
            // the definition is gone, so match the calls by name; skip names another definition still answers for
            if g.by_name.get(name).map_or(false, |v| !v.is_empty()) {
                continue;
            }
            uses = g.called.get(name).into_iter().flatten().filter(|(f, _)| g.reachable(f, &rel)).map(|(f, i)| { let s = &g.files[f]; let c = &s.calls[*i]; Ref { file: f.clone(), in_def: c.from.map(|d| s.defs[d].name.clone()), line: c.line } }).collect();
        }
        uses.retain(|r| r.in_def.as_deref() != Some(name));
        if uses.is_empty() {
            continue;
        }
        let list = uses.iter().take(8).map(|r| format!("{}:{}{}", r.file, r.line, r.in_def.as_ref().map_or(String::new(), |d| format!(" ({d})")))).collect::<Vec<_>>().join(", ");
        let more = if uses.len() > 8 { format!(", and {} more (graph callers {name})", uses.len() - 8) } else { String::new() };
        rows.push(format!("[graph: the signature of `{name}` {what} and it is used in {} other place(s): {list}{more}. Update them, or check they still fit.]", uses.len()));
    }
    (!rows.is_empty()).then(|| rows.join("\n"))
}

impl SymGraph {
    /// Files outside `files` that call something defined inside them, with the names they call: what a change can break.
    /// The source lines of one definition, for hashing or showing.
    pub fn body(cwd: &Path, file: &str, d: &Def) -> String {
        std::fs::read_to_string(cwd.join(file)).unwrap_or_default().lines().skip(d.line - 1).take(d.end - d.line + 1).collect::<Vec<_>>().join("\n")
    }

    pub fn impact(&self, files: &[String], limit: usize) -> Vec<(String, Vec<String>)> {
        let want: std::collections::HashSet<&str> = files.iter().map(|f| f.trim_start_matches("./")).collect();
        let mut by_file: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> = Default::default();
        for f in want.iter() {
            let Some(s) = self.files.get(*f) else { continue };
            for d in s.defs.iter().filter(|d| !matches!(d.kind, "impl" | "mod")) {
                for r in self.callers(f, &d.name) {
                    if !want.contains(r.file.as_str()) {
                        by_file.entry(r.file).or_default().insert(d.name.clone());
                    }
                }
            }
        }
        let mut rows: Vec<(String, Vec<String>)> = by_file.into_iter().map(|(f, n)| (f, n.into_iter().collect())).collect();
        rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
        rows.into_iter().take(limit).map(|(f, n)| (f, n.into_iter().take(4).collect())).collect()
    }
}

impl SymGraph {
    /// Definitions whose name is spelled out in the request ("fix parse_invoice totals" -> parse_invoice), best first:
    /// exact identifiers, then multi-word names whose every word the request uses, ranked by how many places call them.
    pub fn mentioned(&self, request: &str, limit: usize) -> Vec<(String, Def, usize)> {
        use crate::util::rank::tokenize;
        let words: std::collections::HashSet<&str> = request.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).filter(|w| w.len() >= 3).collect();
        let want: std::collections::HashSet<String> = tokenize(request).into_iter().collect();
        let heads: Vec<String> = want.iter().filter(|t| t.len() >= 3).map(|t| t.chars().take(4).collect()).collect();
        let mut found: Vec<(f64, String, Def, usize)> = vec![];
        for (name, at) in &self.by_name {
            // plain short words (run, get, load) show up in ordinary sentences, so only identifier-looking names count as written
            let exact = words.contains(name.as_str()) && (name.len() >= 6 || name.contains('_') || name.chars().any(char::is_uppercase));
            if !exact {
                let low = name.to_lowercase();
                if !heads.iter().any(|h| low.contains(h.as_str())) {
                    continue;
                }
                let parts = tokenize(name);
                // a one-word name like `run` or `get` is too common to guess at; it has to be written exactly
                let distinct: std::collections::HashSet<&String> = parts.iter().collect();
                if distinct.len() < 2 || !parts.iter().all(|p| want.contains(p)) {
                    continue;
                }
            }
            for (f, i) in at {
                let d = &self.files[f].defs[*i];
                if matches!(d.kind, "impl" | "mod") {
                    continue;
                }
                let callers = self.callers(f, name).len();
                let score = if exact { 100.0 } else { 10.0 } + (1.0 + callers as f64).ln();
                found.push((score, f.clone(), d.clone(), callers));
            }
        }
        found.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)).then(a.2.line.cmp(&b.2.line)));
        found.into_iter().take(limit).map(|(_, f, d, c)| (f, d, c)).collect()
    }
}

/// A few lines for the model about the code the request names: where it is defined, how many places call it, and what
/// the developer said that area is for. None when the request names nothing the graph knows.
pub fn task_map(cwd: &Path, request: &str) -> Option<String> {
    let g = load(cwd);
    let hits = g.mentioned(request, MAP_ROWS);
    if hits.is_empty() {
        return None;
    }
    let cfg = crate::util::graphcfg::load(cwd);
    let mut rows: Vec<String> = vec![];
    for (n, (f, d, c)) in hits.iter().enumerate() {
        rows.push(format!("- {} {} at {f}:{}-{} ({c} call site{})", d.kind, d.name, d.line, d.end, if *c == 1 { "" } else { "s" }));
        if let Some(note) = cfg.note_for(cwd, f, d) {
            rows.push(format!("    {note}"));
        }
        // the first few also get their neighbours, so the model starts with the surrounding code already mapped
        if n < 3 {
            let short = |v: Vec<String>| if v.is_empty() { None } else { Some(v.into_iter().take(4).collect::<Vec<_>>().join(", ")) };
            let mut from: Vec<String> = g.callers(f, &d.name).into_iter().map(|r| format!("{}:{}", r.file, r.line)).collect();
            from.dedup();
            if let Some(s) = short(from) {
                rows.push(format!("    used from: {s}"));
            }
            if let Some(s) = short(g.callees(f, &d.name).into_iter().filter(|(_, x)| matches!(x.kind, "function" | "method")).map(|(cf, x)| format!("{} ({cf}:{})", x.name, x.line)).collect()) {
                rows.push(format!("    calls: {s}"));
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for (f, _, _) in &hits {
        if let Some((dir, note)) = cfg.areas.iter().filter(|(k, _)| f.starts_with(&format!("{k}/"))).max_by_key(|(k, _)| k.len()) {
            if seen.insert(dir.clone()) {
                rows.push(format!("- area {dir}/: {note}"));
            }
        }
    }
    Some(format!("[Code graph: definitions your request names (live, matched by name; read the code before relying on it; graph symbol/trace go deeper)\n{}]", rows.join("\n")))
}

/// One line for a grep whose pattern is the name of something defined here, e.g. `parse_invoice` or `\bparse_invoice\(`:
/// "[graph: function parse_invoice is defined at billing/invoice.py:1-2, used in 2 places; ...]". None for any other pattern,
/// for a name defined in more than four places, and in repos too small to be offered the graph tool. A name with two to
/// four definitions gets a line that tells them apart instead.
pub fn grep_hint(cwd: &Path, pattern: &str) -> Option<String> {
    let name = pattern.trim_start_matches("\\b").trim_end_matches("\\(").trim_end_matches('(').trim_end_matches("\\b");
    if name.len() < 4 || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let g = load(cwd);
    let defs: Vec<&(String, usize)> = g.by_name.get(name)?.iter().filter(|(f, i)| !matches!(g.files[f].defs[*i].kind, "impl" | "mod")).collect();
    if g.files.len() < 15 || defs.is_empty() || defs.len() > 4 {
        return None;
    }
    if let [(f, i)] = defs[..] {
        let d = &g.files[f].defs[*i];
        return Some(format!("[graph: {} {name} is defined at {f}:{}-{}, used in {} place(s). `graph callers {name}` lists them without the noise, `graph symbol {name}` reads just its body.]", d.kind, d.line, d.end, g.callers(f, name).len()));
    }
    // Two unrelated things share the name (billing/charge.py and games/charge.py both define `charge`): grep mixes
    // them together, the graph keeps each one's users apart.
    let each: Vec<String> = defs.iter().map(|(f, i)| format!("{f}:{} ({} use(s))", g.files[f].defs[*i].line, g.callers(f, name).len())).collect();
    Some(format!("[graph: {} different definitions are named {name}: {}. These matches mix them; `graph callers {name}` lists the users of each one separately.]", defs.len(), each.join(", ")))
}

pub fn listed(cwd: &Path) -> Vec<String> {
    let Ok(o) = std::process::Command::new("git").args(["ls-files", "--cached", "--others", "--exclude-standard"]).current_dir(cwd).output() else { return vec![] };
    let cfg = crate::util::graphcfg::load(cwd);
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|f| Path::new(f).extension().map_or(false, |e| EXTS.contains(&e.to_string_lossy().to_lowercase().as_str())))
        .filter(|f| !crate::util::graphcfg::is_noise(f) && !cfg.is_ignored(f))
        .take(MAX_FILES)
        .map(String::from)
        .collect()
}

fn stamp(p: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(p).ok()?;
    if m.len() > MAX_BYTES {
        return None;
    }
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() as u64;
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

fn cache_file(root: &Path) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let h = hex::encode(Sha256::digest(root.to_string_lossy().as_bytes()));
    crate::config::loader::get_data_dir().join("graph").join(format!("{}.json", &h[..16]))
}

const CACHE_VERSION: u32 = 3; // 2: calls carry what they were written after, and types / JSX / handed-over functions count as uses

/// What the last session parsed, if anything. A bad or old file is just ignored: we re-parse and overwrite it.
fn read_disk(root: &Path) -> HashMap<String, Entry> {
    let Ok(text) = std::fs::read_to_string(cache_file(root)) else { return HashMap::new() };
    let Ok((v, files)) = serde_json::from_str::<(u32, HashMap<String, (Stamp, FileSyms)>)>(&text) else { return HashMap::new() };
    if v != CACHE_VERSION {
        return HashMap::new();
    }
    files.into_iter().map(|(f, (st, s))| (f, (st, Arc::new(s)))).collect()
}

/// Saved off the calling thread, so an edit never waits on writing the whole repo's graph. Rename keeps readers from
/// ever seeing half a file. ponytail: rewrites everything; per-file shards if repos get big enough for that to hurt.
fn write_disk(root: &Path, parsed: &HashMap<String, Entry>) {
    let path = cache_file(root);
    let snapshot: Vec<(String, Entry)> = parsed.iter().map(|(f, e)| (f.clone(), e.clone())).collect();
    std::thread::spawn(move || {
        let files: HashMap<&String, (&Stamp, &FileSyms)> = snapshot.iter().map(|(f, (st, s))| (f, (st, s.as_ref()))).collect();
        let Ok(text) = serde_json::to_string(&(CACHE_VERSION, files)) else { return };
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    });
}

/// Build the whole repo's graph in the background at startup so the first question finds it ready.
pub fn warm(cwd: std::path::PathBuf) {
    std::thread::spawn(move || {
        load(&cwd);
    });
}

type Cache = HashMap<String, (HashMap<String, Entry>, Option<(u64, Arc<SymGraph>)>)>;

/// The graph for this working tree. Unchanged files come from the cache; only new or edited ones are parsed.
pub fn load(cwd: &Path) -> Arc<SymGraph> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    // One build at a time: the background warm-up and an early graph question must share the work, not both parse the repo.
    static GATE: Mutex<()> = Mutex::new(());
    let _one_at_a_time = GATE.lock().unwrap_or_else(|e| e.into_inner());
    let cwd = resolve(cwd);
    let key = cwd.to_string_lossy().to_string();
    let cache = C.get_or_init(|| Mutex::new(HashMap::new()));
    let (mut parsed, last) = cache.lock().unwrap().remove(&key).unwrap_or_default();
    if parsed.is_empty() {
        parsed = read_disk(&cwd);
    }

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
    let changed = !todo.is_empty();
    parsed.extend(parse_all(&cwd, todo));
    let names: std::collections::HashSet<&String> = live.iter().map(|(f, _)| f).collect();
    parsed.retain(|f, _| names.contains(f)); // deleted files drop out
    if changed || parsed.len() != live.len() {
        write_disk(&cwd, &parsed);
    }
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
        // `x` is there because it is handed to parse(): a function passed by name counts as a use
        assert_eq!(by, [("parse".into(), Some(0)), ("x".into(), Some(0)), ("load".into(), Some(1)), ("send".into(), Some(1)), ("load".into(), Some(3))]);
        assert_eq!(s.calls[3].qual.as_deref(), Some(".api"));
        assert_eq!(s.defs[3].parent.as_deref(), Some("Router"));
    }

    #[test]
    fn python_rust_go() {
        let py = extract("a.py", "class A:\n    def go(self):\n        helper()\n").unwrap();
        assert_eq!(names(&py), ["class:A", "method:go"]);
        assert_eq!(py.calls[0].name, "helper");
        let rs = extract("a.rs", "struct S;\nimpl S { fn new() -> S { make(); S } }\nfn make() {}").unwrap();
        assert_eq!(names(&rs), ["struct:S", "impl:S", "method:new", "function:make"]);
        assert!(rs.calls.iter().any(|c| c.name == "make" && c.qual.is_none()));
        let go = extract("a.go", "package a\nfunc (r *R) Do() { fmt.Println(helper()) }\nfunc helper() int { return 1 }").unwrap();
        assert_eq!(names(&go), ["method:Do", "function:helper"]);
        assert_eq!(go.defs[0].parent.as_deref(), Some("R"));
        assert!(go.calls.iter().any(|c| c.name == "helper") && go.calls.iter().any(|c| c.name == "Println"));
    }

    #[test]
    fn java_csharp_php_ruby() {
        let calls = |s: &FileSyms| s.calls.iter().map(|c| format!("{}{}", c.qual.as_deref().map_or(String::new(), |q| format!("{q}>")), c.name)).collect::<Vec<_>>();
        let java = extract("A.java", "class Shop {\n  Shop() {}\n  void pay(Cart c) { c.total(); Tax.rate(); helper(); new Receipt(); }\n  void helper() {}\n}").unwrap();
        assert_eq!(names(&java), ["class:Shop", "method:Shop", "method:pay", "method:helper"]);
        let got = calls(&java);
        for want in [".c>total", ".Tax>rate", "helper", "Receipt", "Cart"] {
            assert!(got.contains(&want.to_string()), "java {want} in {got:?}");
        }
        let cs = extract("A.cs", "class Shop {\n  void Pay() { cart.Total(); Helper(); var r = new Receipt(); }\n  void Helper() {}\n}").unwrap();
        assert_eq!(names(&cs), ["class:Shop", "method:Pay", "method:Helper"]);
        let got = calls(&cs);
        for want in [".cart>Total", "Helper", "Receipt"] {
            assert!(got.contains(&want.to_string()), "c# {want} in {got:?}");
        }
        let php = extract("a.php", "<?php\nclass Shop {\n  function pay($c) { $c->total(); Tax::rate(); helper(); new Receipt(); }\n}\nfunction helper() {}\n").unwrap();
        assert_eq!(names(&php), ["class:Shop", "method:pay", "function:helper"]);
        let got = calls(&php);
        for want in [".c>total", "Tax>rate", "helper", "Receipt"] {
            assert!(got.contains(&want.to_string()), "php {want} in {got:?}");
        }
        let rb = extract("a.rb", "class Shop\n  def pay(c)\n    c.total()\n    Tax.rate()\n    helper()\n  end\n  def helper\n  end\nend\n").unwrap();
        assert_eq!(names(&rb), ["class:Shop", "method:pay", "method:helper"]);
        let got = calls(&rb);
        for want in [".c>total", ".Tax>rate", "helper"] {
            assert!(got.contains(&want.to_string()), "ruby {want} in {got:?}");
        }
    }

    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::process::Command::new("git").args(["init", "-q"]).current_dir(d.path()).status().unwrap();
        for (f, text) in files {
            std::fs::create_dir_all(d.path().join(f).parent().unwrap()).unwrap();
            std::fs::write(d.path().join(f), text).unwrap();
        }
        d
    }

    fn who(g: &SymGraph, file: &str, name: &str) -> Vec<String> {
        g.callers(file, name).into_iter().map(|r| format!("{}:{}", r.file, r.line)).collect()
    }

    /// Rust has no import graph here, so three functions all called `load` are told apart by how each call is written.
    #[test]
    fn rust_calls_go_to_the_load_they_name() {
        let d = repo(&[
            ("src/cache.rs", "pub fn load() {}\n"),
            ("src/config.rs", "pub struct Config;\nimpl Config {\n    pub fn load() -> Config { Config }\n}\npub fn load() {}\n"),
            ("src/app.rs", "use crate::cache::load;\nfn a() { load(); }\nfn b() { crate::config::load(); }\nfn c(x: Config) { Config::load(); x.len(); }\n"),
            ("src/list.rs", "pub struct List;\nimpl List {\n    pub fn len(&self) -> usize { 0 }\n}\n"),
        ]);
        let g = load(d.path());
        assert_eq!(who(&g, "src/cache.rs", "load"), ["src/app.rs:2"], "the one the `use` line brought in");
        let cfg = g.callers("src/config.rs", "load");
        assert_eq!(cfg.iter().map(|r| r.line).collect::<Vec<_>>(), [3, 4], "the module's function and the type's method, not cache's");
        // a type is used where it is mentioned, even though nobody "calls" it
        assert_eq!(who(&g, "src/config.rs", "Config"), ["src/app.rs:4", "src/config.rs:3"]);
        assert_eq!(g.impact(&["src/config.rs".into()], 5)[0].0, "src/app.rs");
        assert!(who(&g, "src/list.rs", "len").is_empty(), "x.len() could be any value's len");
    }

    /// A React component is run by rendering it, a handler by passing it, and both often arrive through an index file.
    #[test]
    fn components_callbacks_and_barrel_imports_count_as_uses() {
        let d = repo(&[
            ("ui/Nav.tsx", "export function Nav() { return null; }\n"),
            ("ui/index.ts", "export { Nav } from './Nav';\n"),
            ("lib/save.ts", "export function save() {}\n"),
            ("app.tsx", "import { Nav } from './ui';\nimport { save } from './lib/save';\nexport function App() {\n  return <div onClick={save}><Nav /></div>;\n}\n"),
        ]);
        let g = load(d.path());
        assert_eq!(who(&g, "ui/Nav.tsx", "Nav"), ["app.tsx:4"]);
        assert_eq!(who(&g, "lib/save.ts", "save"), ["app.tsx:4"]);
    }

    #[test]
    fn grep_for_a_known_name_points_at_the_graph() {
        let mut files: Vec<(String, String)> = (0..15).map(|i| (format!("m{i}.py"), format!("def filler_{i}():\n    pass\n"))).collect();
        files.push(("lib.py".into(), "def parse_invoice(x):\n    return x\n".into()));
        files.push(("app.py".into(), "from lib import parse_invoice\nparse_invoice(1)\n".into()));
        files.push(("billing/charge.py".into(), "def charge(a):\n    return a\n".into()));
        files.push(("games/charge.py".into(), "def charge(b):\n    return b\n".into()));
        files.push(("pay.py".into(), "from billing.charge import charge\ncharge(1)\n".into()));
        let d = repo(&files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect::<Vec<_>>());
        let h = grep_hint(d.path(), "\\bparse_invoice\\(").unwrap();
        assert!(h.contains("function parse_invoice is defined at lib.py:1-2, used in 1 place(s)"), "{h}");
        // same name, two unrelated functions: the hint says so and counts each one's users apart
        let h = grep_hint(d.path(), "charge").unwrap();
        assert!(h.contains("2 different definitions are named charge: billing/charge.py:1 (1 use(s)), games/charge.py:1 (0 use(s))"), "{h}");
        assert!(grep_hint(d.path(), "parse.*invoice").is_none() && grep_hint(d.path(), "nothing_here").is_none());
    }

    /// route -> service -> repo, three files. One trace call answers "what does changing save() reach" and one path
    /// call answers "how does the route get to save()", where grep would need a round per hop.
    #[test]
    fn trace_and_path_follow_several_hops_and_an_edit_names_the_callers_it_affects() {
        let d = repo(&[
            ("repo.py", "def save(x):\n    return x\n"),
            ("service.py", "from repo import save\n\ndef charge(x):\n    return save(x)\n"),
            ("route.py", "from service import charge\n\ndef pay(x):\n    return charge(x)\n"),
        ]);
        let p = d.path();
        let g = load(p);
        let up: Vec<String> = g.trace("repo.py", "save", true, 3, 20).into_iter().map(|(hop, f, n, _)| format!("{hop} {f} {n}")).collect();
        assert_eq!(up, ["1 service.py charge", "2 route.py pay"]);
        let down: Vec<String> = g.trace("route.py", "pay", false, 3, 20).into_iter().map(|(hop, _, n, _)| format!("{hop} {n}")).collect();
        assert_eq!(down, ["1 charge", "2 save"]);
        let chain: Vec<String> = g.path("route.py", "pay", "save").unwrap().into_iter().map(|(_, n, _)| n).collect();
        assert_eq!(chain, ["pay", "charge", "save"]);
        assert!(g.path("repo.py", "save", "pay").is_none(), "calls only run downwards");

        // save() grows a parameter: the edit result itself says who has to change
        let (old, new) = ("def save(x):\n    return x\n", "def save(x, retries):\n    return x\n");
        std::fs::write(p.join("repo.py"), new).unwrap();
        let note = edit_note(p, &p.join("repo.py"), old, new).unwrap();
        assert!(note.contains("signature of `save` changed") && note.contains("service.py:4 (charge)"), "{note}");
        // a body-only change is nobody else's business
        assert!(edit_note(p, &p.join("repo.py"), new, "def save(x, retries):\n    return [x]\n").is_none());
    }

    #[test]
    fn a_note_stays_with_its_symbol_and_is_marked_once_the_code_changes() {
        let d = repo(&[("pay.py", "def charge(x):\n    return x\n")]);
        let p = d.path();
        let def = load(p).find("charge", 1).remove(0).1;
        let mut cfg = crate::util::graphcfg::load(p);
        cfg.set_note(p, "pay.py", &def, "retries twice, then queues");
        crate::util::graphcfg::save(p, &cfg).unwrap();
        let cfg = crate::util::graphcfg::load(p);
        assert_eq!(cfg.note_for(p, "pay.py", &def).unwrap(), "note: retries twice, then queues");
        std::fs::write(p.join("pay.py"), "def charge(x):\n    return x + 1\n").unwrap();
        assert!(cfg.note_for(p, "pay.py", &def).unwrap().contains("code changed after this note"));
    }

    #[test]
    fn saved_graph_reads_back_the_same() {
        let s = extract("a.rs", "struct S;\nimpl S { fn new() { make(); } }\nfn make() {}").unwrap();
        let text = serde_json::to_string(&(CACHE_VERSION, HashMap::from([("a.rs", ((3u64, 9u64), &s))]))).unwrap();
        let (v, back): (u32, HashMap<String, (Stamp, FileSyms)>) = serde_json::from_str(&text).unwrap();
        let b = &back["a.rs"].1;
        assert_eq!((v, back["a.rs"].0), (CACHE_VERSION, (3, 9)));
        assert_eq!(names(b), names(&s));
        assert_eq!(b.calls[0].name, "make");
    }

    #[test]
    fn task_map_names_definitions_callers_and_area_notes() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::process::Command::new("git").args(["init", "-q"]).current_dir(p).status().unwrap();
        std::fs::create_dir_all(p.join("billing")).unwrap();
        std::fs::write(p.join("billing/invoice.py"), "def parse_invoice(x):\n    return x\n\ndef run():\n    pass\n").unwrap();
        std::fs::write(p.join("app.py"), "from billing.invoice import parse_invoice\n\ndef main():\n    parse_invoice(1)\n    parse_invoice(2)\n").unwrap();
        crate::util::graphcfg::save(p, &crate::util::graphcfg::GraphCfg { areas: [("billing".to_string(), "Stripe invoices".to_string())].into(), ..Default::default() }).unwrap();
        let m = task_map(p, "the totals are wrong, fix parse_invoice").unwrap();
        assert!(m.contains("function parse_invoice at billing/invoice.py:1-2 (2 call sites)") && m.contains("area billing/: Stripe invoices"), "{m}");
        // words in different order still find a multi-word name; a bare common word does not
        assert!(task_map(p, "invoice parse is broken").unwrap().contains("parse_invoice"));
        assert!(task_map(p, "please run the thing").is_none());
    }

    #[test]
    fn impact_lists_outside_files_that_call_into_the_changed_ones() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::process::Command::new("git").args(["init", "-q"]).current_dir(p).status().unwrap();
        std::fs::write(p.join("lib.py"), "def parse(x):\n    return x\n\ndef dump(x):\n    return x\n").unwrap();
        std::fs::write(p.join("app.py"), "from lib import parse, dump\n\ndef main():\n    parse(1)\n    dump(2)\n").unwrap();
        std::fs::write(p.join("lone.py"), "def solo():\n    pass\n").unwrap();
        let g = load(p);
        assert_eq!(g.impact(&["lib.py".into()], 5), [("app.py".to_string(), vec!["dump".to_string(), "parse".to_string()])]);
        assert!(g.impact(&["lone.py".into()], 5).is_empty());
        assert!(g.impact(&["app.py".into()], 5).is_empty(), "nothing calls main");
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
