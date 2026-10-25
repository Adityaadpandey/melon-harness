//! The tool layer: the `Tool` trait, parameter validation, results, and the registry that every call goes through.
pub mod builtin;
pub mod mcp;
pub mod discovery;
pub mod registry;
pub mod repair;
pub mod subagents;

use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Write,
    Shell,
    Network,
    Memory,
    Mcp,
}

impl ToolKind {
    pub fn value(&self) -> &'static str {
        match self {
            ToolKind::Read => "read",
            ToolKind::Write => "write",
            ToolKind::Shell => "shell",
            ToolKind::Network => "network",
            ToolKind::Memory => "memory",
            ToolKind::Mcp => "mcp",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileDiff {
    pub path: PathBuf,
    pub old_content: String,
    pub new_content: String,
    pub is_new_file: bool,
    pub is_deletion: bool,
}

impl FileDiff {
    pub fn to_diff(&self) -> String {
        let fix = |s: &str| if !s.is_empty() && !s.ends_with('\n') { format!("{s}\n") } else { s.to_string() };
        let (old, new) = (fix(&self.old_content), fix(&self.new_content));
        let from = if self.is_new_file { "/dev/null".to_string() } else { self.path.to_string_lossy().to_string() };
        let to = if self.is_deletion { "/dev/null".to_string() } else { self.path.to_string_lossy().to_string() };
        let diff = similar::TextDiff::from_lines(&old, &new);
        diff.unified_diff().context_radius(3).header(&from, &to).to_string()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    pub metadata: serde_json::Map<String, Value>,
    pub truncated: bool,
    pub diff: Option<FileDiff>,
    pub exit_code: Option<i32>,
}

impl ToolResult {
    pub fn error(error: impl Into<String>) -> ToolResult {
        ToolResult { success: false, output: String::new(), error: Some(error.into()), ..Default::default() }
    }
    pub fn error_with(error: impl Into<String>, meta: Value) -> ToolResult {
        let mut r = ToolResult::error(error);
        r.metadata = meta.as_object().cloned().unwrap_or_default();
        r
    }
    pub fn ok(output: impl Into<String>) -> ToolResult {
        ToolResult { success: true, output: output.into(), ..Default::default() }
    }
    pub fn ok_with(output: impl Into<String>, meta: Value) -> ToolResult {
        let mut r = ToolResult::ok(output);
        r.metadata = meta.as_object().cloned().unwrap_or_default();
        r
    }
    pub fn to_model_output(&self) -> String {
        if self.success {
            self.output.clone()
        } else {
            format!("Error: {}\n\nOutput:\n{}", self.error.clone().unwrap_or_default(), self.output)
        }
    }
    pub fn flag(&self, key: &str) -> bool {
        self.metadata.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
}

pub type ProgressFn = Arc<dyn Fn(&str, Value) + Send + Sync>;

#[derive(Clone)]
pub struct ToolInvocation {
    pub params: Value,
    pub cwd: PathBuf,
    /// Bumps when earlier tool output leaves the context (see ReadFileTool).
    pub epoch: u64,
    pub call_id: String,
    /// Long-running tools report live status here.
    pub progress: Option<ProgressFn>,
}

impl ToolInvocation {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.params.get(key).and_then(Value::as_str)
    }
}

#[derive(Clone, Debug)]
pub struct ToolConfirmation {
    pub tool_name: String,
    pub params: Value,
    pub description: String,
    pub diff: Option<FileDiff>,
    pub affected_paths: Vec<PathBuf>,
    pub command: Option<String>,
    pub is_dangerous: bool,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> String;
    fn kind(&self) -> ToolKind;
    /// JSON schema of the arguments: `{"type": "object", "properties": {...}, "required": [...]}`.
    fn schema(&self) -> Value;
    fn as_any(&self) -> &dyn Any;

    fn is_mutating(&self, _params: &Value) -> bool {
        matches!(self.kind(), ToolKind::Write | ToolKind::Shell | ToolKind::Network | ToolKind::Memory)
    }

    async fn get_confirmation(&self, inv: &ToolInvocation) -> Option<ToolConfirmation> {
        if !self.is_mutating(&inv.params) {
            return None;
        }
        Some(ToolConfirmation { tool_name: self.name().into(), params: inv.params.clone(), description: format!("Execute {}", self.name()), diff: None, affected_paths: vec![], command: None, is_dangerous: false })
    }

    async fn execute(&self, inv: &ToolInvocation) -> ToolResult;

    /// Check (and gently coerce) the arguments against the schema; errors read like "Parameter 'path': Field required".
    fn validate_params(&self, params: &Value) -> Result<Value, Vec<String>> {
        validate(&self.schema(), params)
    }

    fn to_openai_schema(&self) -> Value {
        let s = self.schema();
        json!({"name": self.name(), "description": self.description(),
               "parameters": {"type": "object", "properties": s.get("properties").cloned().unwrap_or(json!({})), "required": s.get("required").cloned().unwrap_or(json!([]))}})
    }
}

// ------------------------------------------------------------------------------------------------ validation

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn phrase(t: &str) -> &'static str {
    match t {
        "string" => "Input should be a valid string",
        "integer" => "Input should be a valid integer",
        "number" => "Input should be a valid number",
        "boolean" => "Input should be a valid boolean",
        "array" => "Input should be a valid list",
        "object" => "Input should be a valid dictionary",
        _ => "Input should be null",
    }
}

fn allowed_types(schema: &Value) -> Vec<String> {
    match &schema["type"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => vec![],
    }
}

/// Coerce one value toward `want` the way pydantic's lax mode does ("5" -> 5, 1 -> true).
fn coerce(v: &Value, want: &str) -> Option<Value> {
    match (want, v) {
        ("integer", Value::String(s)) => s.trim().parse::<i64>().ok().map(|n| json!(n)),
        ("integer", Value::Number(n)) if n.as_f64().map_or(false, |f| f.fract() == 0.0) => n.as_f64().map(|f| json!(f as i64)),
        ("number", Value::String(s)) => s.trim().parse::<f64>().ok().map(|n| json!(n)),
        ("number", Value::Number(_)) => Some(v.clone()),
        ("boolean", Value::String(s)) => match s.to_lowercase().as_str() {
            "true" | "1" | "yes" | "on" | "t" | "y" => Some(json!(true)),
            "false" | "0" | "no" | "off" | "f" | "n" => Some(json!(false)),
            _ => None,
        },
        ("boolean", Value::Number(n)) => match n.as_i64() {
            Some(1) => Some(json!(true)),
            Some(0) => Some(json!(false)),
            _ => None,
        },
        ("string", Value::Number(_)) | ("string", Value::Bool(_)) => None, // pydantic does not turn numbers into strings
        _ => None,
    }
}

fn check(schema: &Value, v: &Value, loc: &str, errors: &mut Vec<String>) -> Value {
    let types = allowed_types(schema);
    let mut value = v.clone();
    if !types.is_empty() {
        let actual = type_name(v);
        let ok = types.iter().any(|t| t == actual || (t == "number" && actual == "integer"));
        if !ok {
            let coerced = types.iter().find_map(|t| coerce(v, t));
            match coerced {
                Some(c) => value = c,
                None => {
                    let first = types.iter().find(|t| *t != "null").map(String::as_str).unwrap_or("null");
                    errors.push(format!("Parameter '{loc}': {}", phrase(first)));
                    return value;
                }
            }
        }
    }
    if let Some(en) = schema["enum"].as_array() {
        if !en.contains(&value) {
            let opts = en.iter().map(|e| e.as_str().map(|s| format!("'{s}'")).unwrap_or_else(|| e.to_string())).collect::<Vec<_>>().join(", ");
            errors.push(format!("Parameter '{loc}': Input should be {opts}"));
        }
    }
    if let Some(n) = value.as_f64() {
        if let Some(m) = schema["minimum"].as_f64() {
            if n < m {
                errors.push(format!("Parameter '{loc}': Input should be greater than or equal to {m}"));
            }
        }
        if let Some(m) = schema["maximum"].as_f64() {
            if n > m {
                errors.push(format!("Parameter '{loc}': Input should be less than or equal to {m}"));
            }
        }
    }
    if let Value::Array(items) = &value {
        if let Some(m) = schema["minItems"].as_u64() {
            if (items.len() as u64) < m {
                errors.push(format!("Parameter '{loc}': List should have at least {m} item{} after validation, not {}", if m == 1 { "" } else { "s" }, items.len()));
            }
        }
        if let Some(m) = schema["maxItems"].as_u64() {
            if items.len() as u64 > m {
                errors.push(format!("Parameter '{loc}': List should have at most {m} item{} after validation, not {}", if m == 1 { "" } else { "s" }, items.len()));
            }
        }
        if schema["items"].is_object() {
            value = Value::Array(items.iter().enumerate().map(|(i, it)| check(&schema["items"], it, &format!("{loc}.{i}"), errors)).collect());
        }
    }
    if let Value::Object(obj) = &value {
        if let Some(props) = schema["properties"].as_object() {
            let mut out = obj.clone();
            for req in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if !obj.contains_key(req) {
                    errors.push(format!("Parameter '{loc}.{req}': Field required"));
                }
            }
            for (k, ps) in props {
                if let Some(x) = obj.get(k) {
                    if x.is_null() && !schema["required"].as_array().map_or(false, |r| r.iter().any(|q| q == k)) {
                        continue; // None for an optional field
                    }
                    let nv = check(ps, x, &format!("{loc}.{k}"), errors);
                    out.insert(k.clone(), nv);
                }
            }
            value = Value::Object(out);
        }
    }
    value
}

pub fn validate(schema: &Value, params: &Value) -> Result<Value, Vec<String>> {
    if !params.is_object() {
        return Err(vec!["Arguments must be an object".into()]);
    }
    let mut errors = vec![];
    let mut props_only = schema.clone();
    // top-level errors read "Parameter 'name'" with no "root." prefix
    let v = check(&props_only, params, "", &mut errors);
    let _ = &mut props_only;
    let errors: Vec<String> = errors.into_iter().map(|e| e.replacen("'.", "'", 1)).collect();
    if errors.is_empty() { Ok(v) } else { Err(errors) }
}

/// Helper for building schemas tersely.
pub fn schema_of(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_like_pydantic() {
        let s = schema_of(json!({"path": {"type": "string"}, "limit": {"type": "integer", "minimum": 1}, "all": {"type": "boolean"}}), &["path"]);
        assert_eq!(validate(&s, &json!({"path": "a", "limit": "5"})).unwrap()["limit"], 5);
        let e = validate(&s, &json!({"limit": 0})).unwrap_err();
        assert!(e.iter().any(|x| x == "Parameter 'path': Field required"), "{e:?}");
        assert!(e.iter().any(|x| x.contains("greater than or equal to 1")));
        assert!(validate(&s, &json!({"path": 3})).is_err());
        assert_eq!(validate(&s, &json!({"path": "a", "all": "true"})).unwrap()["all"], true);
        assert!(validate(&s, &json!({"path": "a", "limit": null})).is_ok());
    }
}
