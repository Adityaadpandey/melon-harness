use crate::tools::{schema_of, FileDiff, Tool, ToolConfirmation, ToolInvocation, ToolKind, ToolResult};
use crate::util::edits::plan_edits;
use crate::util::paths::{atomic_write, ensure_parent_directory, resolve_path};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::path::PathBuf;

pub struct EditTool;

/// (path, old content, new content, error, replacement count): shared by preview and execution.
type Plan = (PathBuf, String, Option<String>, Option<String>, usize);

/// newline="" semantics: keep CRLF intact; lossy utf-8 like the Python reader.
fn read(path: &std::path::Path) -> String {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default()
}

impl EditTool {
    fn pairs(p: &Value) -> Vec<(String, String)> {
        if let Some(edits) = p["edits"].as_array().filter(|e| !e.is_empty()) {
            return edits.iter().map(|e| (e["old_string"].as_str().unwrap_or("").to_string(), e["new_string"].as_str().unwrap_or("").to_string())).collect();
        }
        vec![(p["old_string"].as_str().unwrap_or("").to_string(), p["new_string"].as_str().unwrap_or("").to_string())]
    }

    fn plan(inv: &ToolInvocation) -> Plan {
        let path = resolve_path(&inv.cwd, inv.str("path").unwrap_or(""));
        let new_string = inv.str("new_string").unwrap_or("").to_string();
        let has_edits = inv.params["edits"].as_array().map_or(false, |e| !e.is_empty());
        if !path.exists() {
            if !inv.str("old_string").unwrap_or("").is_empty() && !has_edits {
                return (path.clone(), String::new(), None, Some(format!("File does not exist: {}. To create a file, leave old_string empty (or use write_file).", path.display())), 0);
            }
            return (path, String::new(), Some(new_string), None, 1); // new file
        }
        if !path.is_file() {
            return (path.clone(), String::new(), None, Some(format!("Not a file: {}", path.display())), 0);
        }
        let old = read(&path);
        match plan_edits(&old, &Self::pairs(&inv.params), inv.params["replace_all"].as_bool().unwrap_or(false)) {
            Ok((new, n)) => (path, old, Some(new), None, n),
            Err(e) => (path, old, None, Some(e), 0),
        }
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> String {
        "Edit a file by exact text replacement. Use `edits` for several changes in one call. Trailing-whitespace and smart-quote differences are tolerated; anything else must match exactly. The file must have been read first. `old_string` must be unique: add surrounding lines to make it so, or set replace_all to rename across the file. Copy it without the line-number prefix that read_file shows. To create or fully rewrite a file use write_file.".into()
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
                "path": {"type": "string", "description": "File to edit (relative to the working directory, or absolute)"},
                "old_string": {"type": "string", "default": "", "description": "Exact text to replace. Empty only when creating a new file."},
                "new_string": {"type": "string", "default": "", "description": "Replacement text. Empty deletes the matched text."},
                "replace_all": {"type": "boolean", "default": false, "description": "Replace every occurrence of old_string"},
                "edits": {"type": ["array", "null"], "description": "Several replacements in ONE call (preferred when changing multiple places in a file). Each is matched against the original file, must be unique, and must not overlap another. When given, old_string/new_string are ignored.",
                          "items": {"type": "object", "properties": {
                              "old_string": {"type": "string", "description": "Exact text to replace; unique in the file"},
                              "new_string": {"type": "string", "description": "Replacement text (may be empty to delete)"}}, "required": ["old_string", "new_string"]}},
            }),
            &["path"],
        )
    }

    async fn get_confirmation(&self, inv: &ToolInvocation) -> Option<ToolConfirmation> {
        let (path, old, new, err, _) = Self::plan(inv);
        if err.is_some() {
            return None; // nothing will be written, so there is nothing to approve; execute() reports the error
        }
        let is_new = !path.exists();
        Some(ToolConfirmation {
            tool_name: "edit".into(),
            params: inv.params.clone(),
            description: format!("{} {}", if is_new { "Create" } else { "Edit" }, path.display()),
            diff: Some(FileDiff { path: path.clone(), old_content: old, new_content: new.unwrap_or_default(), is_new_file: is_new, is_deletion: false }),
            affected_paths: vec![path],
            command: None,
            is_dangerous: false,
        })
    }

    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let (path, old, new, err, n) = Self::plan(inv);
        if let Some(e) = err {
            return ToolResult::error(e);
        }
        let new = new.unwrap_or_default();
        let is_new = !path.exists();
        if let Err(e) = ensure_parent_directory(&path).and_then(|_| atomic_write(&path, &new)) {
            return ToolResult::error(format!("Failed to write file: {e}"));
        }
        let delta = crate::util::search::split_lines(&new).len() as i64 - crate::util::search::split_lines(&old).len() as i64;
        let note = if delta != 0 { format!(" ({delta:+} lines)") } else { String::new() };
        let verb = if is_new { "Created".to_string() } else { format!("Edited ({n} replacement{})", if n != 1 { "s" } else { "" }) };
        let mut r = ToolResult::ok_with(
            format!("{verb} {}{note}", path.display()),
            json!({"path": path.to_string_lossy(), "replaced_count": n, "line_diff": delta, "is_new_file": is_new}),
        );
        r.diff = Some(FileDiff { path, old_content: old, new_content: new, is_new_file: is_new, is_deletion: false });
        r
    }
}
