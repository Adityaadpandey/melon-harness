use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::paths::resolve_path;
use crate::util::re::Re;
use crate::util::search::{list_files, search, similar_paths};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;

const MAX_GREP_CHARS: usize = 7000; // about 2k tokens

pub struct GrepTool {
    config: ConfigHandle,
}
impl GrepTool {
    pub fn new(config: ConfigHandle) -> Self {
        GrepTool { config }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> String {
        "Search file contents. Respects .gitignore, finds dotfiles, never reads secrets. Output: path:line:text (context lines use '-'). Always use this instead of grep or rg in the shell. Use files_only to locate, then read what matters. Literal braces and dots need escaping in a regex (`interface\\{\\}`), or set literal=true. For open-ended hunts that need several rounds, ask a sub-agent.".into()
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
                "pattern": {"type": "string", "description": "Regex (or literal text with literal=true) to search for"},
                "path": {"type": "string", "default": ".", "description": "File or directory to search (default: working directory)"},
                "glob": {"type": ["string", "null"], "description": "Only search files matching this glob, e.g. '*.ts' or 'src/**/*.spec.ts'"},
                "case_insensitive": {"type": ["boolean", "null"], "description": "Default is smart case: insensitive unless the pattern has an uppercase letter"},
                "literal": {"type": "boolean", "default": false, "description": "Treat pattern as plain text, not a regex"},
                "context": {"type": "integer", "minimum": 0, "maximum": 10, "default": 0, "description": "Lines of context before and after each match"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 100, "description": "Maximum matches to return (default 100)"},
                "files_only": {"type": "boolean", "default": false, "description": "Return only the files that match, with counts (cheap way to locate things)"},
            }),
            &["pattern"],
        )
    }

    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let cfg = self.config.get();
        let pattern = inv.str("pattern").unwrap_or("").to_string();
        let target = resolve_path(&inv.cwd, inv.str("path").unwrap_or("."));
        if !target.exists() {
            return ToolResult::error(format!("Path does not exist: {}.{}", target.display(), similar_paths(&inv.cwd, &target, &cfg.forbidden_paths)));
        }
        let literal = inv.params["literal"].as_bool().unwrap_or(false);
        let ignore_case = inv.params["case_insensitive"].as_bool().unwrap_or_else(|| !pattern.chars().any(|c| c.is_uppercase()));
        let pat = if literal { regex::escape(&pattern) } else { pattern.clone() };
        let regex = match Re::new(&pat, ignore_case) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("Invalid regex: {e}. Use literal=true for plain text.")),
        };
        let root = if target.is_dir() { target.clone() } else { target.parent().unwrap().to_path_buf() };
        let files = if target.is_dir() {
            let (r, f) = (root.clone(), cfg.forbidden_paths.clone());
            tokio::task::spawn_blocking(move || list_files(&r, &f)).await.unwrap_or_default()
        } else {
            vec![target.clone()]
        };
        let context = inv.params["context"].as_u64().unwrap_or(0) as usize;
        let limit = inv.params["limit"].as_u64().unwrap_or(100) as usize;
        let glob = inv.str("glob").map(String::from);
        let files_only = inv.params["files_only"].as_bool().unwrap_or(false);
        let n_files = files.len();
        let res = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || search(&files, &root, &regex, context, limit, glob.as_deref(), files_only)).await.unwrap()
        };
        let meta = json!({"matches": res.total, "files_with_matches": res.hit_files, "files_searched": n_files});
        if !files_only && res.total > limit && res.hit_files > 15 {
            // too broad to read line by line: show where matches live instead (from the same pass, no second read)
            let mut lines: Vec<String> = res.per_file.iter().map(|(r, n)| format!("{r} ({n})")).collect();
            lines.push(format!("{} matches across {} files is too broad to list. Narrow with path/glob or a more specific pattern, then read.", res.total, res.hit_files));
            return ToolResult::ok_with(lines.join("\n"), meta);
        }
        if res.lines.is_empty() {
            return ToolResult::ok_with(format!("No matches for '{pattern}' in {n_files} files"), meta);
        }
        let mut lines = res.lines;
        if !files_only && res.total > limit {
            lines.push(format!("... {} more matches in {} files not shown; narrow the pattern, path or glob", res.total - limit, res.hit_files));
        }
        let mut body = lines.join("\n");
        if body.chars().count() > MAX_GREP_CHARS {
            // wide context or long lines: cap by size, not just by match count
            let cut: String = body.chars().take(MAX_GREP_CHARS).collect();
            body = format!("{}\n... output clipped; narrow the pattern, path or glob, or use files_only", cut.rsplit_once('\n').map_or(cut.as_str(), |x| x.0));
        }
        ToolResult::ok_with(body, meta)
    }
}
