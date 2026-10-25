use crate::tools::{schema_of, FileDiff, Tool, ToolConfirmation, ToolInvocation, ToolKind, ToolResult};
use crate::util::paths::{atomic_write, ensure_parent_directory, resolve_path};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> String {
        "Write content to a file. Creates the file if it doesn't exist, or overwrites if it does. Parent directories are created automatically. Use this for creating new files or completely replacing file contents. For partial modifications, use the edit tool instead. An existing file must be read first. Do not create docs or README files unless asked.".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> Value {
        schema_of(
            json!({
                "path": {"type": "string", "description": "Path to the file to write (relative to working directory or absolute)"},
                "content": {"type": "string", "description": "Content to write to the file"},
                "create_directories": {"type": "boolean", "default": true, "description": "Create parent directories if they don't exist"},
            }),
            &["path", "content"],
        )
    }

    async fn get_confirmation(&self, inv: &ToolInvocation) -> Option<ToolConfirmation> {
        let path = resolve_path(&inv.cwd, inv.str("path")?);
        let is_new = !path.exists();
        let old = if is_new { String::new() } else { std::fs::read_to_string(&path).unwrap_or_default() };
        Some(ToolConfirmation {
            tool_name: "write_file".into(),
            params: inv.params.clone(),
            description: format!("{} file: {}", if is_new { "Created" } else { "Updated" }, path.display()),
            diff: Some(FileDiff { path: path.clone(), old_content: old, new_content: inv.str("content").unwrap_or("").to_string(), is_new_file: is_new, is_deletion: false }),
            affected_paths: vec![path],
            command: None,
            is_dangerous: !is_new,
        })
    }

    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let path = resolve_path(&inv.cwd, inv.str("path").unwrap_or(""));
        let mut content = inv.str("content").unwrap_or("").to_string();
        let create_dirs = inv.params["create_directories"].as_bool().unwrap_or(true);
        let is_new = !path.exists();
        let old = if is_new { String::new() } else { std::fs::read_to_string(&path).unwrap_or_default() };
        // formatters (biome, prettier, black) fail a file that ends without "\n"; models often forget it
        if !content.is_empty() && !content.ends_with('\n') && (is_new || old.ends_with('\n')) {
            content.push('\n');
        }
        if create_dirs {
            if let Err(e) = ensure_parent_directory(&path) {
                return ToolResult::error(format!("Failed to write file: {e}"));
            }
        } else if !path.parent().map_or(true, |p| p.exists()) {
            return ToolResult::error(format!("Parent directory does not exist: {}", path.parent().unwrap().display()));
        }
        if let Err(e) = atomic_write(&path, &content) {
            return ToolResult::error(format!("Failed to write file: {e}"));
        }
        let lines = crate::util::search::split_lines(&content).len();
        let mut r = ToolResult::ok_with(
            format!("{} {} {lines} lines", if is_new { "Created" } else { "Updated" }, path.display()),
            json!({"path": path.to_string_lossy(), "is_new_file": is_new, "lines": lines, "bytes": content.len()}),
        );
        r.diff = Some(FileDiff { path, old_content: old, new_content: content, is_new_file: is_new, is_deletion: false });
        r
    }
}
