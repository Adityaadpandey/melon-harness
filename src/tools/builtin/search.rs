use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::paths::resolve_path;
use crate::util::rank::rank_files;
use crate::util::search::list_files;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;

pub struct SearchTool {
    config: ConfigHandle,
}
impl SearchTool {
    pub fn new(config: ConfigHandle) -> Self {
        SearchTool { config }
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }
    fn description(&self) -> String {
        "Find which files are about something. Ranks files by relevance to a plain-words query (BM25 over file paths and contents, camelCase aware) and shows the best matching lines of each. Use it FIRST when you do not know where something lives; use grep afterwards for exact strings.".into()
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
                "query": {"type": "string", "description": "What you are looking for, in plain words or identifiers, e.g. 'event description blank line section markdown'"},
                "path": {"type": "string", "default": ".", "description": "Directory to search (default: working directory)"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 20, "default": 8, "description": "How many files to return"},
            }),
            &["query"],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let cfg = self.config.get();
        let query = inv.str("query").unwrap_or("").to_string();
        let root = resolve_path(&inv.cwd, inv.str("path").unwrap_or("."));
        if !root.is_dir() {
            return ToolResult::error(format!("Directory does not exist: {}", root.display()));
        }
        let limit = inv.params["limit"].as_u64().unwrap_or(8) as usize;
        let forbidden = cfg.forbidden_paths.clone();
        let prior = crate::agent::learn::Learned::new(&cfg.cwd).prior(&query);
        let (r, q) = (root.clone(), query.clone());
        let (files_n, ranked) = tokio::task::spawn_blocking(move || {
            let files = list_files(&r, &forbidden);
            let ranked = rank_files(&r, &files, &q, limit, 3, Some(&prior));
            (files.len(), ranked)
        })
        .await
        .unwrap();
        if ranked.is_empty() {
            return ToolResult::ok(format!("No files match '{query}' in {files_n} files. Try other words or grep."));
        }
        let mut out = vec![];
        for h in &ranked {
            out.push(format!("{}  ({})", h.path, h.score));
            out.extend(h.snippets.iter().map(|(n, t)| format!("  {n}: {t}")));
        }
        ToolResult::ok_with(out.join("\n"), json!({"files": ranked.len(), "searched": files_n}))
    }
}
