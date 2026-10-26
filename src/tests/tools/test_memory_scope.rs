//! Ported from tests/test_memory_scope.py: the memory tool is per workspace, and what it stores reaches the prompt.
use super::{handle, invocation};
use crate::tools::builtin::memory::MemoryTool;
use crate::tools::Tool;
use serde_json::{json, Value};
use std::path::Path;

async fn run(cwd: &Path, params: Value) -> String {
    MemoryTool::new(handle(cwd)).execute(&invocation(cwd, params)).await.output
}

#[tokio::test]
async fn memory_is_per_workspace() {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("shop"), d.path().join("blog"));
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    run(&a, json!({"action": "set", "key": "test_cmd", "value": "npm test"})).await;
    assert!(run(&a, json!({"action": "get", "key": "test_cmd"})).await.contains("npm test"));
    assert!(run(&b, json!({"action": "get", "key": "test_cmd"})).await.contains("not found"));
    assert!(run(&b, json!({"action": "list"})).await.contains("No memories"));
}

#[tokio::test]
async fn stored_memory_reaches_the_prompt() {
    // the tool and the prompt used to read different files, so nothing the model saved ever came back
    let d = tempfile::tempdir().unwrap();
    run(d.path(), json!({"action": "set", "key": "test_cmd", "value": "npm test"})).await;
    let prompt = crate::agent::session::load_memory(&MemoryTool::new(handle(d.path()))).unwrap();
    assert!(prompt.contains("test_cmd: npm test"), "{prompt}");
}
