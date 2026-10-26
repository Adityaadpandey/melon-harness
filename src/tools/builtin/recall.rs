use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::archive::ResultStore;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::sync::{Arc, Mutex};

pub struct RecallResultTool {
    pub store: Mutex<Arc<ResultStore>>,
}
impl RecallResultTool {
    pub fn new(store: Arc<ResultStore>) -> Self {
        RecallResultTool { store: Mutex::new(store) }
    }
}

#[async_trait]
impl Tool for RecallResultTool {
    fn name(&self) -> &str {
        "recall_result"
    }
    fn description(&self) -> String {
        "Get back the exact text of an earlier tool result that was cleared from context or cut short (its note shows an id like r7). Returns a line range; use offset/limit to page through a long one.".into()
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
                "id": {"type": "string", "description": "Result id from a note like [Old tool result cleared (r7 ...)] or [... full output: r7]"},
                "offset": {"type": "integer", "minimum": 1, "default": 1, "description": "First line to return (1-based)"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 400, "default": 200, "description": "Max lines to return"},
            }),
            &["id"],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let id = inv.str("id").unwrap_or("");
        let offset = inv.params["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = inv.params["limit"].as_u64().unwrap_or(200) as usize;
        let Some(text) = self.store.lock().unwrap().get(id) else {
            return ToolResult::error(format!("No archived result '{id}'. Re-run the original tool instead."));
        };
        let lines = crate::util::search::split_lines(&text);
        let part: Vec<&str> = lines.iter().skip(offset - 1).take(limit).cloned().collect();
        let end = offset - 1 + part.len();
        let tail = if end < lines.len() { format!("\n[lines {offset}-{end} of {}; pass offset={} for more]", lines.len(), end + 1) } else { String::new() };
        ToolResult::ok(format!("{}{tail}", part.join("\n")))
    }
}
