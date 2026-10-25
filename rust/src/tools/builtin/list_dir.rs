use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::paths::resolve_path;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;

pub struct ListDirTool;

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn description(&self) -> String {
        "List contents of a directory".into()
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
                "path": {"type": "string", "default": ".", "description": "Directory path to list (default: current directory)"},
                "include_hidden": {"type": "boolean", "default": false, "description": "Whether to include hidden files and directories (default: false"},
            }),
            &[],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let dir = resolve_path(&inv.cwd, inv.str("path").unwrap_or("."));
        if !dir.is_dir() {
            return ToolResult::error(format!("Directory does not exist: {}", dir.display()));
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Error listing directory: {e}")),
        };
        let mut items: Vec<(bool, String)> = rd.flatten().map(|e| (e.path().is_dir(), e.file_name().to_string_lossy().to_string())).collect();
        items.sort_by(|a, b| (!a.0, a.1.to_lowercase()).cmp(&(!b.0, b.1.to_lowercase())));
        if !inv.params["include_hidden"].as_bool().unwrap_or(false) {
            items.retain(|(_, n)| !n.starts_with('.'));
        }
        if items.is_empty() {
            return ToolResult::ok_with("Directory is empty", json!({"path": dir.to_string_lossy(), "entries": 0}));
        }
        let lines: Vec<String> = items.iter().map(|(d, n)| if *d { format!("{n}/") } else { n.clone() }).collect();
        ToolResult::ok_with(lines.join("\n"), json!({"path": dir.to_string_lossy(), "entries": items.len()}))
    }
}
