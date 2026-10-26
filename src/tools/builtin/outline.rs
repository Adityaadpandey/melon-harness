use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::outline::outline;
use crate::util::paths::resolve_path;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;

pub struct OutlineTool;

#[async_trait]
impl Tool for OutlineTool {
    fn name(&self) -> &str {
        "outline"
    }
    fn description(&self) -> String {
        "List the functions, classes, types and methods of a file with their line ranges (about 30 tokens per symbol instead of the whole file). Then read one with read_file(path, symbol='name') or an offset/limit slice. Use it on any file over ~150 lines before reading.".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> Value {
        schema_of(json!({"path": {"type": "string", "description": "Source file to outline"}}), &["path"])
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let path = resolve_path(&inv.cwd, inv.str("path").unwrap_or(""));
        if !path.is_file() {
            return ToolResult::error(format!("File not found: {}", path.display()));
        }
        let src = String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()).to_string();
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let syms = outline(&name, &src);
        let total = crate::util::search::split_lines(&src).len();
        if syms.is_empty() {
            return ToolResult::ok(format!("{total} lines, no symbols recognised. Read it with offset/limit."));
        }
        let rows: Vec<String> = syms
            .iter()
            .take(200)
            .map(|s| format!("{}-{}  {}{} {}", s.start, s.end, "  ".repeat(if s.indent < 8 { s.indent / 2 } else { 4 }), s.kind, s.name))
            .collect();
        let more = if syms.len() > 200 { format!("\n... {} more symbols", syms.len() - 200) } else { String::new() };
        ToolResult::ok(format!("{name}: {total} lines\n{}{more}", rows.join("\n")))
    }
}
