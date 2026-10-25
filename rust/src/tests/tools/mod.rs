//! Ported from Python tests (see each file's header).
use crate::config::ConfigHandle;
use crate::tools::{Tool, ToolInvocation, ToolResult};
use serde_json::Value;
use std::path::Path;

/// A config handle rooted at `cwd`, the way `Config(cwd=repo)` is used in the Python tests.
pub(super) fn handle(cwd: &Path) -> ConfigHandle {
    ConfigHandle::new(super::fakes::config(cwd))
}

/// Run a tool the way the Python `run(tool, cwd, **params)` helper does.
pub(super) async fn run_tool(tool: &dyn Tool, cwd: &Path, params: Value) -> ToolResult {
    tool.execute(&invocation(cwd, params)).await
}

pub(super) fn invocation(cwd: &Path, params: Value) -> ToolInvocation {
    ToolInvocation { params, cwd: cwd.to_path_buf(), epoch: 0, call_id: "t1".into(), progress: None }
}

/// `git <args>` in `dir`, failing the test if git does.
pub(super) fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

mod test_search_edit;
mod test_search_listing;
mod test_tool_repair;
mod test_tool_schemas;
mod test_graph;
mod test_importgraph;
mod test_repomap;
mod test_diagnostics;
mod test_verify_js;
mod test_templates;
mod test_rewind;
mod test_notes;
mod test_memory_scope;
mod test_task_modes;
mod test_task_verify;
mod test_workspace;
