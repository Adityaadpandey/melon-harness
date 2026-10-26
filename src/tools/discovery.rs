//! Project tools without Python: every `.melon/tools/*.toml` (in the repo and in the config dir) becomes a tool whose
//! implementation is a shell command. The arguments reach it as JSON on stdin and as MELON_ARG_<NAME> variables.
//!
//!     # .melon/tools/lint_sql.toml
//!     name = "lint_sql"
//!     description = "Lint a SQL file with sqlfluff"
//!     command = "sqlfluff lint \"$MELON_ARG_PATH\""
//!     kind = "read"            # read (default, runs unasked) or shell (asks like any command)
//!     [parameters.path]
//!     type = "string"
//!     description = "file to lint"
//!     required = true
use super::{Tool, ToolInvocation, ToolKind, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

struct ScriptTool {
    name: String,
    description: String,
    command: String,
    kind: ToolKind,
    schema: Value,
}

#[async_trait]
impl Tool for ScriptTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> String {
        self.description.clone()
    }
    fn kind(&self) -> ToolKind {
        self.kind
    }
    fn schema(&self) -> Value {
        self.schema.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&self.command).current_dir(&inv.cwd).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        for (k, v) in inv.params.as_object().into_iter().flatten() {
            let text = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
            cmd.env(format!("MELON_ARG_{}", k.to_uppercase()), text);
        }
        let Ok(mut child) = cmd.spawn() else { return ToolResult::error(format!("could not start: {}", self.command)) };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(inv.params.to_string().as_bytes()).await;
        }
        let out = match tokio::time::timeout(std::time::Duration::from_secs(120), child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return ToolResult::error(e.to_string()),
            Err(_) => return ToolResult::error("timed out after 120s"),
        };
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let code = out.status.code().unwrap_or(-1);
        let mut r = if out.status.success() { ToolResult::ok(text) } else { ToolResult { output: text, error: Some(format!("exit code {code}")), ..Default::default() } };
        r.exit_code = Some(code);
        r
    }
}

fn load(file: &Path) -> Option<Arc<dyn Tool>> {
    let t: toml::Value = toml::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    let name = t.get("name")?.as_str()?.to_string();
    let command = t.get("command")?.as_str()?.to_string();
    let (mut props, mut required) = (serde_json::Map::new(), vec![]);
    for (k, p) in t.get("parameters").and_then(|p| p.as_table()).into_iter().flatten() {
        props.insert(k.clone(), json!({"type": p.get("type").and_then(|v| v.as_str()).unwrap_or("string"), "description": p.get("description").and_then(|v| v.as_str()).unwrap_or("")}));
        if p.get("required").and_then(|v| v.as_bool()).unwrap_or(false) {
            required.push(json!(k));
        }
    }
    Some(Arc::new(ScriptTool {
        name,
        description: t.get("description").and_then(|v| v.as_str()).unwrap_or("project tool").to_string(),
        command,
        kind: if t.get("kind").and_then(|v| v.as_str()) == Some("shell") { ToolKind::Shell } else { ToolKind::Read },
        schema: json!({"type": "object", "properties": props, "required": required}),
    }))
}

/// Tools from `<dir>/.melon/tools/*.toml` (or `<dir>/tools/*.toml` for the config dir).
pub fn discover(dir: &Path) -> Vec<Arc<dyn Tool>> {
    let mut files: Vec<_> = std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x == "toml")).collect()).unwrap_or_default();
    files.sort();
    files.iter().filter_map(|f| load(f)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_script_tool() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("t.toml"), "name='hi'\ncommand='echo $MELON_ARG_WHO'\n[parameters.who]\ntype='string'\nrequired=true\n").unwrap();
        let tools = discover(d.path());
        assert_eq!(tools[0].name(), "hi");
        let r = tokio::runtime::Runtime::new().unwrap().block_on(tools[0].execute(&ToolInvocation { params: json!({"who": "mel"}), cwd: d.path().into(), epoch: 0, call_id: String::new(), progress: None }));
        assert_eq!(r.output.trim(), "mel");
    }
}
