use crate::agent::orchestrator::{parse_graph, GraphError, Orchestrator};
use crate::config::{ApprovalPolicy, ConfigHandle};
use crate::tools::{schema_of, Tool, ToolConfirmation, ToolInvocation, ToolKind, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;

pub struct ParallelTasksTool {
    config: ConfigHandle,
}
impl ParallelTasksTool {
    pub fn new(config: ConfigHandle) -> Self {
        ParallelTasksTool { config }
    }
}

#[async_trait]
impl Tool for ParallelTasksTool {
    fn name(&self) -> &str {
        "parallel_tasks"
    }
    fn description(&self) -> String {
        "Run 2+ coding tasks at the same time, each worker in its own git worktree, then merge them in dependency order and run the project checks. Use ONLY for work on different files that does not depend on each other; for exploration use sub-agents instead. Conflicts are detected and reported, never silently resolved.".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> Value {
        let item = json!({"type": "object", "properties": {
            "id": {"type": "string", "description": "Short unique id"},
            "goal": {"type": "string", "description": "Complete, self-contained instruction for one worker"},
            "files": {"type": "array", "items": {"type": "string"}, "description": "Files this worker owns (it must not edit others)"},
            "deps": {"type": "array", "items": {"type": "string"}, "description": "Ids of tasks that must finish first"}}, "required": ["id", "goal"]});
        schema_of(
            json!({"tasks": {"type": "array", "minItems": 2, "items": item, "description": "2 or more tasks"},
                   "apply": {"type": "boolean", "default": true, "description": "Fast-forward the merged result into the working tree when it is clean"}}),
            &["tasks"],
        )
    }
    async fn get_confirmation(&self, inv: &ToolInvocation) -> Option<ToolConfirmation> {
        let tasks = inv.params["tasks"].as_array()?;
        let rows: Vec<String> = tasks
            .iter()
            .map(|t| {
                let files: Vec<&str> = t["files"].as_array().map(|f| f.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                format!("  {}: {}{}", t["id"].as_str().unwrap_or(""), crate::util::text::head_chars(t["goal"].as_str().unwrap_or(""), 110), if files.is_empty() { String::new() } else { format!("  [{}]", files.join(", ")) })
            })
            .collect();
        Some(ToolConfirmation {
            tool_name: "parallel_tasks".into(),
            params: inv.params.clone(),
            description: format!("Run {} tasks in parallel (separate git worktrees):\n{}", tasks.len(), rows.join("\n")),
            diff: None,
            affected_paths: vec![inv.cwd.clone()],
            command: None,
            is_dangerous: false,
        })
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let tasks = inv.params["tasks"].clone();
        let apply = inv.params["apply"].as_bool().unwrap_or(true);
        let nodes = match parse_graph(&json!({"tasks": tasks})) {
            Ok(n) => n,
            Err(GraphError(e)) => return ToolResult::error(format!("Invalid task graph: {e}")),
        };
        // the user approved this plan, so workers may edit inside their own worktree; no recursion
        let mut cfg = (*self.config.get()).clone();
        cfg.approval = ApprovalPolicy::AutoEdit;
        cfg.enable_parallel_tasks = false;
        cfg.subagents = vec![];
        cfg.auto_review = false;
        let request = nodes.iter().map(|n| n.goal.clone()).collect::<Vec<_>>().join("; ");
        let workers = cfg.max_parallel_agents;
        let res = match Orchestrator::new(cfg, workers, inv.progress.clone()).run(&request, nodes, apply).await {
            Ok(r) => r,
            Err(GraphError(e)) => return ToolResult::error(e),
        };
        let mut lines = vec![format!("Parallel run: {} in {}s (sum of worker time {}s)", res["status"].as_str().unwrap_or(""), res["wall_sec"], res["sum_worker_sec"])];
        for t in res["tasks"].as_array().into_iter().flatten() {
            let changed: Vec<&str> = t["changed"].as_array().map(|c| c.iter().filter_map(Value::as_str).take(6).collect()).unwrap_or_default();
            let files = if changed.is_empty() { "no changes".to_string() } else { changed.join(", ") };
            let note = t["note"].as_str().filter(|n| !n.is_empty()).map(|n| format!("; {n}")).unwrap_or_default();
            lines.push(format!("- {}: {} ({}s) {files}{note}", t["id"].as_str().unwrap_or(""), t["status"].as_str().unwrap_or(""), t["seconds"]));
        }
        if res["overlapping_files"].as_object().map_or(false, |o| !o.is_empty()) {
            lines.push(format!("Files edited by several workers: {}", res["overlapping_files"]));
        }
        for c in res["combined_checks"].as_array().into_iter().flatten() {
            lines.push(format!("check {}: {}", c["command"].as_str().unwrap_or(""), if c["ok"] == true { "ok" } else { "FAILED" }));
        }
        lines.push(format!("Integration branch: {}; applied to the working tree: {}", res["integration_branch"].as_str().unwrap_or(""), res["applied"].as_str().unwrap_or("")));
        if res["status"] == "ok" { ToolResult::ok(lines.join("\n")) } else { ToolResult::error(lines.join("\n")) }
    }
}
