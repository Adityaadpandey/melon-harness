use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::{codegraph, importgraph};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::collections::HashSet;
use std::path::Path;

const LIMIT: usize = 25;

pub struct GraphTool {
    _config: ConfigHandle,
}
impl GraphTool {
    pub fn new(config: ConfigHandle) -> Self {
        GraphTool { _config: config }
    }

    fn live(act: &str, name: Option<&str>, cwd: &Path) -> ToolResult {
        let Some(name) = name else { return ToolResult::error("`name` (a file path or file name) is required for this action") };
        let ig = importgraph::load(cwd);
        let (rel, maybe) = ig.find_file(name);
        let Some(rel) = rel else {
            return ToolResult::ok(format!("No single file matches '{name}'.{}", if maybe.is_empty() { " Use glob to find the path.".into() } else { format!(" Did you mean: {}", maybe.join(", ")) }));
        };
        match act {
            "imports" => {
                let mut rows: Vec<&String> = ig.out.get(&rel).map(|s| s.iter().collect()).unwrap_or_default();
                rows.sort();
                let body = rows.iter().take(LIMIT * 2).map(|r| format!("  {r}")).collect::<Vec<_>>().join("\n");
                ToolResult::ok(format!("{rel} imports {} project file(s):\n{body}", rows.len()))
            }
            "importers" => {
                let mut rows: Vec<&String> = ig.inc.get(&rel).map(|s| s.iter().collect()).unwrap_or_default();
                rows.sort();
                if rows.is_empty() {
                    return ToolResult::ok(format!("No file imports {rel} (an entry point, a route file the framework loads by name, or dead code)."));
                }
                ToolResult::ok(format!("{} file(s) import {rel}:\n{}", rows.len(), rows.iter().take(LIMIT * 2).map(|r| format!("  {r}")).collect::<Vec<_>>().join("\n")))
            }
            "tests" => {
                let rows = ig.tests_for(&rel);
                if rows.is_empty() {
                    return ToolResult::ok(format!("No test file imports or is named after {rel}."));
                }
                ToolResult::ok(format!("tests that cover {rel}:\n{}", rows.iter().map(|r| format!("  {r}")).collect::<Vec<_>>().join("\n")))
            }
            _ => {
                let radius = ig.blast_radius(&rel, 3, 40); // blast, or impact without a graph snapshot
                if radius.is_empty() {
                    return ToolResult::ok(format!("Nothing imports {rel}, directly or through other files. Framework entry files (page.tsx, route.ts, layout.tsx) are loaded by name, not imported."));
                }
                let body = radius.iter().map(|(f, h)| format!("  {} {f}", "·".repeat(*h))).collect::<Vec<_>>().join("\n");
                ToolResult::ok(format!("depends on {rel} (indented by import hops: · direct, ·· via one file, ...):\n{body}\nRead the call sites of the direct ones before changing behaviour."))
            }
        }
    }
}

#[async_trait]
impl Tool for GraphTool {
    fn name(&self) -> &str {
        "graph"
    }
    fn description(&self) -> String {
        "Understand how the code connects. importers/imports/blast/tests are computed live from the working tree (always current): who imports a file, what breaks if it changes, which tests cover it. find/callers/callees/path need a prebuilt graph snapshot (graphify-out/graph.json) and are pointers to verify with read_file. One call replaces several grep/read rounds.".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> Value {
        schema_of(
            json!({
                "action": {"type": "string", "description": "find (locate symbols/files by name) | callers (who calls NAME; direction checked in source) | callees (what NAME calls) | importers (files that import FILE, live from the working tree; follows @/ aliases) | imports (what FILE imports) | blast (everything that depends on FILE, nearest first) | tests (tests that cover FILE) | neighbors (all relations of NAME) | impact (files that depend on file/symbol NAME: what a change could break) | path (shortest connection NAME -> TO) | community (the module NAME belongs to) | stats"},
                "name": {"type": ["string", "null"], "description": "Symbol, class or file name (e.g. delTicketTypeCache, ticket.service.ts, apps/web/src/components/ui/DesktopNav.tsx)"},
                "to": {"type": ["string", "null"], "description": "Second name, for action=path"},
            }),
            &["action"],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let act = inv.str("action").unwrap_or("").to_lowercase();
        let name = inv.str("name").filter(|n| !n.is_empty());
        let g = codegraph::load(&inv.cwd);
        if matches!(act.as_str(), "importers" | "imports" | "blast" | "tests") || (act == "impact" && g.is_none()) {
            return Self::live(&act, name, &inv.cwd);
        }
        let Some(g) = g else { return ToolResult::error("No code graph found (graphify-out/graph.json). Use grep and read_file instead.") };
        let note = match g.staleness() {
            Some(s) => format!("\n\n({s}; verify with read_file)"),
            None => "\n\n(from the code graph snapshot; verify with read_file)".into(),
        };
        if act == "stats" {
            return ToolResult::ok(format!("{} nodes, {} edges.{note}", g.nodes.len(), g.out.values().map(Vec::len).sum::<usize>()));
        }
        let Some(name) = name else { return ToolResult::error("`name` is required for this action") };
        if act == "impact" {
            let (node, _) = g.resolve(name);
            let target = node.map(|n| g.rel(g.nodes[&n]["source_file"].as_str().unwrap_or(""))).unwrap_or_else(|| name.to_string());
            let rows = g.impact(&[target.clone()], 15);
            if rows.is_empty() {
                return ToolResult::ok(format!("No other file depends on {target} in the graph.{note}"));
            }
            let body = rows.iter().map(|(f, l)| format!("  {f}  ({})", l.join(", "))).collect::<Vec<_>>().join("\n");
            return ToolResult::ok(format!("files that depend on {target} (read their call sites before changing its behaviour):\n{body}{note}"));
        }
        if act == "find" {
            let hits = g.find(name, LIMIT);
            if hits.is_empty() {
                return ToolResult::ok(format!("No graph node matches '{name}'. Try grep."));
            }
            let rows: Vec<String> = hits.iter().map(|h| format!("{:<40} {:<8} {}  [module {}, {} links]", g.label(h), codegraph::kind_of(&g.nodes[h]), g.where_(h), g.nodes[h]["community"].to_string().trim_matches('"'), g.degree(h))).collect();
            return ToolResult::ok(format!("{}{note}", rows.join("\n")));
        }
        let (node, others) = g.resolve(name);
        let Some(node) = node else { return ToolResult::ok(format!("No graph node named '{name}'. Try action=find with part of the name, or grep.")) };
        let head = format!("{} at {}{}", g.label(&node), g.where_(&node), if others.is_empty() { String::new() } else { format!("  (also matches: {})", others.iter().map(|o| g.label(o)).collect::<Vec<_>>().join(", ")) });
        let fmt = |pairs: &[(String, Value)], arrow: &str| -> Vec<String> {
            let mut seen = HashSet::new();
            let mut rows = vec![];
            for (other, e) in pairs {
                if !seen.insert((other.clone(), e["relation"].to_string())) {
                    continue;
                }
                let mark = if e["confidence"] == "INFERRED" { "~" } else { " " };
                rows.push(format!("{mark} {:<10} {arrow} {:<38} {}", e["relation"].as_str().unwrap_or("?"), g.label(other), g.where_(other)));
            }
            rows
        };
        let empty = vec![];
        let (outs, incs) = (g.out.get(&node).unwrap_or(&empty), g.inc.get(&node).unwrap_or(&empty));
        match act.as_str() {
            "callers" | "callees" => {
                // the graph's edge orientation is unreliable, so direction is decided from the source text
                let mut linked: Vec<(String, Value)> = vec![];
                for (other, e) in outs.iter().chain(incs.iter()) {
                    if e["relation"] == "calls" && *other != node && !linked.iter().any(|l| l.0 == *other) {
                        linked.push((other.clone(), e.clone()));
                    }
                }
                let want = if act == "callers" { "caller" } else { "callee" };
                let (mut rows, mut unclear) = (vec![], vec![]);
                for (other, e) in &linked {
                    let d = g.call_direction(&node, other);
                    let mark = if e["confidence"] == "INFERRED" { "~" } else { " " };
                    let line = format!("{mark} {:<38} {}", g.label(other), g.where_(other));
                    if d == want {
                        rows.push(line);
                    } else if d == "unclear" {
                        unclear.push(line);
                    }
                }
                let what = if act == "callers" { "call it" } else { "it calls" };
                let mut out = format!("{head}\n{} that {what} (direction confirmed in the source text):\n{}", rows.len(), rows.iter().take(LIMIT).cloned().collect::<Vec<_>>().join("\n"));
                if !unclear.is_empty() {
                    out.push_str(&format!("\n\ncall-linked but direction unclear ({}); confirm with grep:\n{}", unclear.len(), unclear.iter().take(10).cloned().collect::<Vec<_>>().join("\n")));
                }
                ToolResult::ok(format!("{out}{note}"))
            }
            "neighbors" => {
                let rows: Vec<String> = fmt(outs, "->").into_iter().chain(fmt(incs, "<-")).take(LIMIT * 2).collect();
                ToolResult::ok(format!("{head}\n{}{note}", rows.join("\n")))
            }
            "community" => {
                let cid = g.nodes[&node]["community"].clone();
                let members = g.community(&cid);
                let mut files: Vec<String> = members.iter().map(|m| g.rel(g.nodes[m]["source_file"].as_str().unwrap_or(""))).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                files.truncate(12);
                let rows: Vec<String> = members.iter().take(LIMIT).map(|m| format!("{:<36} {}", g.label(m), g.where_(m))).collect();
                ToolResult::ok(format!("{head}\nmodule {} has {} nodes; main files: {}\ntop members:\n{}{note}", cid.to_string().trim_matches('"'), members.len(), files.join(", "), rows.join("\n")))
            }
            "path" => {
                let Some(to) = inv.str("to").filter(|t| !t.is_empty()) else { return ToolResult::error("`to` is required for action=path") };
                let (other, _) = g.resolve(to);
                let Some(other) = other else { return ToolResult::ok(format!("No graph node named '{to}'.")) };
                match g.path(&node, &other) {
                    None => ToolResult::ok(format!("No connection found between {name} and {to} in the graph.")),
                    Some(chain) => ToolResult::ok(format!("{}{note}", chain.iter().map(|c| format!("{} ({})", g.label(c), g.where_(c))).collect::<Vec<_>>().join(" -> "))),
                }
            }
            _ => ToolResult::error(format!("Unknown action '{}'", inv.str("action").unwrap_or(""))),
        }
    }
}
