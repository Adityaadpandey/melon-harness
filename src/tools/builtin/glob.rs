use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::paths::resolve_path;
use crate::util::search::{glob_match, list_files};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;

const LIMIT: usize = 500;

pub struct GlobTool {
    config: ConfigHandle,
}
impl GlobTool {
    pub fn new(config: ConfigHandle) -> Self {
        GlobTool { config }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }
    fn description(&self) -> String {
        "Find files by name pattern (`*.py`, `src/**/*.ts`). Use it instead of find or ls in the shell. Respects .gitignore and finds dotfiles; never lists secrets. Paths are relative to the search directory.".into()
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
                "pattern": {"type": "string", "description": "Glob like '*.py' (any depth by name) or 'src/**/*.ts' (path pattern)"},
                "path": {"type": "string", "default": ".", "description": "Directory to search in (default: working directory)"},
            }),
            &["pattern"],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let pattern = inv.str("pattern").unwrap_or("").to_string();
        let root = resolve_path(&inv.cwd, inv.str("path").unwrap_or("."));
        if !root.is_dir() {
            return ToolResult::error(format!("Directory does not exist: {}", root.display()));
        }
        let forbidden = self.config.get().forbidden_paths.clone();
        let r = root.clone();
        let files = tokio::task::spawn_blocking(move || list_files(&r, &forbidden)).await.unwrap_or_default();
        let hits: Vec<String> = files
            .iter()
            .map(|f| f.strip_prefix(&root).unwrap_or(f).to_string_lossy().replace('\\', "/"))
            .filter(|rel| glob_match(rel, &pattern))
            .collect();
        if hits.is_empty() {
            return ToolResult::ok_with(format!("No files match '{pattern}'"), json!({"matches": 0}));
        }
        let mut text = hits.iter().take(LIMIT).cloned().collect::<Vec<_>>().join("\n");
        if hits.len() > LIMIT {
            text.push_str(&format!("\n... {} more not shown; use a narrower pattern or path", hits.len() - LIMIT));
        }
        ToolResult::ok_with(text, json!({"matches": hits.len()}))
    }
}
