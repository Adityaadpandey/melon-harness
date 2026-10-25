//! Ported from tests/test_graph.py: the graph tool reads a graphify snapshot and answers find/callers/callees/path/
//! community/impact questions, mapping the snapshot's old absolute paths onto this checkout.
use super::run_tool;
use crate::tools::builtin::graph::GraphTool;
use crate::tools::registry::create_default_registry;
use crate::tools::ToolResult;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

fn n(id: &str, label: &str, f: &str, loc: &str, comm: i64) -> Value {
    json!({"id": id, "label": label, "file_type": "code", "source_file": format!("/Users/someone/old/proj/{f}"), "source_location": loc, "community": comm})
}

fn e(s: &str, t: &str, rel: &str, conf: &str) -> Value {
    json!({"source": s, "target": t, "relation": rel, "confidence": conf})
}

fn w(p: &Path, rel: &str, text: &str) {
    let f = p.join(rel);
    fs::create_dir_all(f.parent().unwrap()).unwrap();
    fs::write(f, text).unwrap();
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    w(p, "src/cache.ts", "export function delTicketTypeCache(id) {\n  return cacheDel(buildKey(id));\n}\n");
    w(p, "src/svc.ts", "class S {\n  async updateTicketType(t) {\n    await delTicketTypeCache(t.eventId);\n  }\n  async deleteTicketType(t) {\n    await delTicketTypeCache(t.eventId);\n  }\n}\n");
    w(p, "src/team.ts", "class T {\n  async reserve() {\n    await this.updateTicketType();\n  }\n}\n");
    let nodes = vec![
        n("cache_del", "delTicketTypeCache()", "src/cache.ts", "L46", 1),
        n("svc_upd", "updateTicketType()", "src/svc.ts", "L270", 2),
        n("svc_del", "deleteTicketType()", "src/svc.ts", "L340", 2),
        n("team_inc", "TeamBookingService.reserve()", "src/team.ts", "L223", 3),
        n("cache_file", "cache.ts", "src/cache.ts", "L1", 1),
    ];
    let links = vec![
        e("svc_upd", "cache_del", "calls", "EXTRACTED"),
        e("svc_del", "cache_del", "calls", "EXTRACTED"),
        e("team_inc", "svc_upd", "calls", "INFERRED"),
        e("cache_file", "cache_del", "contains", "EXTRACTED"),
    ];
    w(p, "graphify-out/graph.json", &json!({"directed": false, "nodes": nodes, "links": links}).to_string());
    d
}

async fn ask(repo: &Path, p: Value) -> ToolResult {
    run_tool(&GraphTool::new(super::handle(repo)), repo, p).await
}

#[tokio::test]
async fn find_and_callers_map_graph_paths_onto_this_checkout() {
    let d = repo();
    let r = ask(d.path(), json!({"action": "find", "name": "TicketType"})).await;
    // the /Users/someone/old/proj/ prefix is stripped
    assert!(r.output.contains("delTicketTypeCache()") && r.output.contains("src/cache.ts:46"), "{}", r.output);
    let r = ask(d.path(), json!({"action": "callers", "name": "delTicketTypeCache"})).await;
    assert!(r.output.contains("2 that call it") && r.output.contains("src/svc.ts:270") && r.output.contains("src/svc.ts:340"), "{}", r.output);
    // a 'contains' edge is not a call
    let rest = r.output.splitn(3, '\n').nth(2).unwrap_or("");
    assert!(!rest.contains("cache.ts"), "{}", r.output);
}

#[tokio::test]
async fn inferred_edges_are_marked_and_callees_path_community_work() {
    let d = repo();
    let p = d.path();
    let r = ask(p, json!({"action": "callees", "name": "TeamBookingService.reserve"})).await;
    // an inferred edge is marked with "~ "
    assert!(r.output.contains("1 that it calls") && r.output.contains("~ ") && r.output.contains("updateTicketType"), "{}", r.output);
    let r = ask(p, json!({"action": "path", "name": "TeamBookingService.reserve", "to": "delTicketTypeCache"})).await;
    let at = |s: &str| r.output.find(s).unwrap_or_else(|| panic!("{s} missing in {}", r.output));
    assert!(at("reserve") < at("updateTicketType") && at("updateTicketType") < at("delTicketTypeCache"));
    let r = ask(p, json!({"action": "community", "name": "updateTicketType"})).await;
    assert!(r.output.contains("module 2 has 2 nodes") && r.output.contains("src/svc.ts"), "{}", r.output);
}

#[tokio::test]
async fn unknown_names_and_missing_graph_fail_helpfully() {
    let d = repo();
    assert!(ask(d.path(), json!({"action": "callers", "name": "doesNotExist"})).await.output.contains("No graph node named"));
    assert!(!ask(d.path(), json!({"action": "callers"})).await.success);
    let empty = tempfile::tempdir().unwrap();
    let r = ask(empty.path(), json!({"action": "find", "name": "x"})).await;
    assert!(!r.success && r.error.unwrap().contains("grep"));
}

#[test]
fn tool_is_only_registered_when_a_graph_exists() {
    let d = repo();
    assert!(create_default_registry(super::handle(d.path())).get("graph").is_some());
    let plain = tempfile::tempdir().unwrap();
    assert!(create_default_registry(super::handle(plain.path())).get("graph").is_none());
}

#[tokio::test]
async fn direction_comes_from_the_source_text_not_the_graphs_edge_order() {
    // The graph stores undirected edges in arbitrary order. Flip one and the answer must not change.
    let d = repo();
    let p = d.path();
    let f = p.join("graphify-out/graph.json");
    let mut g: Value = serde_json::from_str(&fs::read_to_string(&f).unwrap()).unwrap();
    for l in g["links"].as_array_mut().unwrap() {
        let (s, t) = (l["source"].clone(), l["target"].clone());
        l["source"] = t;
        l["target"] = s;
    }
    // the loader caches by file mtime, so make sure the rewrite lands on a new one
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    fs::write(&f, g.to_string()).unwrap();
    let r = ask(p, json!({"action": "callers", "name": "delTicketTypeCache"})).await;
    assert!(r.output.contains("2 that call it"), "{}", r.output);
    let r = ask(p, json!({"action": "callees", "name": "delTicketTypeCache"})).await;
    // cache.ts calls cacheDel/buildKey, which are not in this graph
    assert!(r.output.contains("0 that it calls"), "{}", r.output);
}

#[tokio::test]
async fn impact_lists_files_that_really_call_into_the_changed_file() {
    let d = repo();
    let r = ask(d.path(), json!({"action": "impact", "name": "src/cache.ts"})).await;
    // team.ts only reaches cache.ts through svc.ts
    assert!(r.output.contains("src/svc.ts") && !r.output.contains("src/team.ts"), "{}", r.output);
}
