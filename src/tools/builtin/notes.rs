use crate::agent::notes;
use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;

pub struct NotesTool {
    config: ConfigHandle,
}
impl NotesTool {
    pub fn new(config: ConfigHandle) -> Self {
        NotesTool { config }
    }
}

#[async_trait]
impl Tool for NotesTool {
    fn name(&self) -> &str {
        "notes"
    }
    fn description(&self) -> String {
        "A durable notebook for a long task, kept across context compaction, /resume and new sessions. Write it after a real decision or a milestone (goal, decided, done, next), not after every step. Clear it when the task is finished. For the live checklist use todos.".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Memory
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> Value {
        schema_of(
            json!({
                "action": {"type": "string", "description": "'read', 'write' (replace everything) or 'clear'"},
                "text": {"type": ["string", "null"], "description": "For 'write': the whole notebook, a few short lines (goal, decided, done, next). Max ~3000 chars"},
            }),
            &["action"],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let cwd = self.config.get().cwd.clone();
        match inv.str("action").unwrap_or("").to_lowercase().as_str() {
            "read" => {
                let t = notes::read(&cwd);
                ToolResult::ok(if t.is_empty() { "(no notes yet)".into() } else { t })
            }
            "clear" => {
                notes::write(&cwd, "");
                ToolResult::ok("Notes cleared")
            }
            "write" => match inv.str("text").filter(|t| !t.trim().is_empty()) {
                None => ToolResult::error("`text` is required for 'write'; use 'clear' to empty the notebook"),
                Some(t) => {
                    notes::write(&cwd, t);
                    ToolResult::ok(format!("Notes saved ({} chars)", t.trim().chars().count().min(notes::MAX_CHARS)))
                }
            },
            _ => ToolResult::error("action must be read, write or clear"),
        }
    }
}
