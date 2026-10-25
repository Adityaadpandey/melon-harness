//! Small, safe repairs for tool calls open models get almost right. Claude rarely needs these; DeepSeek, GLM, Qwen and
//! Kimi often send `Read` for read_file, `old_str` for old_string, or a list of edits as a JSON string. Rejecting those
//! costs a whole round trip, so we fix what has exactly one sensible reading and leave everything else to validation.
use serde_json::{Map, Value};
use std::collections::HashSet;

/// Names other harnesses use for our tools; only applied when the real name is offered in this run.
fn tool_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "read" | "view" | "cat" => "read_file",
        "write" | "create_file" => "write_file",
        "edit_file" | "str_replace" | "str_replace_editor" | "multiedit" | "apply_edit" => "edit",
        "bash" | "run" | "run_command" | "execute_command" | "terminal" => "shell",
        "ls" | "list_files" | "list_directory" => "list_dir",
        "find" | "find_files" => "glob",
        "search_files" | "ripgrep" | "rg" => "grep",
        "todo" | "todowrite" | "todo_write" => "todos",
        _ => return None,
    })
}

/// Parameter names from Claude Code, opencode and SWE-agent style tools.
fn param_alias(key: &str) -> Option<&'static str> {
    Some(match key {
        "old_str" | "oldstring" | "old_text" | "search" => "old_string",
        "new_str" | "newstring" | "new_text" | "replace" => "new_string",
        "file_path" | "filepath" | "file" | "filename" | "absolute_path" | "directory" | "dir" => "path",
        "cmd" => "command",
        "replaceall" => "replace_all",
        "line_count" => "limit",
        "start_line" => "offset",
        _ => return None,
    })
}

/// `Read` -> read_file, `Bash` -> shell; an unknown name comes back unchanged so the caller can list the real ones.
pub fn fix_tool_name(name: &str, offered: &HashSet<String>) -> String {
    if name.is_empty() || offered.contains(name) {
        return name.to_string();
    }
    let low = name.to_lowercase();
    if let Some(n) = offered.iter().find(|n| n.to_lowercase() == low) {
        return n.clone();
    }
    match tool_alias(&low.replace('-', "_")) {
        Some(a) if offered.contains(a) => a.to_string(),
        _ => name.to_string(),
    }
}

fn snake(key: &str) -> String {
    let mut out = String::new();
    for (i, c) in key.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// What shapes a field accepts: "array", "object", "string"...; unions list several.
fn kinds(prop: &Value) -> Vec<String> {
    match &prop["type"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().filter(|s| *s != "null").map(String::from)).collect(),
        _ => vec![],
    }
}

/// Rename near-miss keys and undo common encoding slips, only where the schema allows one reading.
/// e.g. {"file_path": "a.py", "old_str": "x", "new_str": "y"} -> {"path": "a.py", "old_string": "x", "new_string": "y"}
///      {"path": "a.py", "edits": "[{\"old_string\": ...}]"} -> edits parsed into a real list.
pub fn fix_arguments(schema: &Value, args: &Map<String, Value>) -> Map<String, Value> {
    let Some(fields) = schema["properties"].as_object() else { return args.clone() };
    let required: Vec<&str> = schema["required"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut out = args.clone();
    for key in args.keys() {
        if fields.contains_key(key) {
            continue;
        }
        let sn = snake(key);
        let target = if fields.contains_key(&sn) { Some(sn) } else { param_alias(&key.to_lowercase()).map(String::from) };
        if let Some(t) = target {
            if fields.contains_key(&t) && !out.contains_key(&t) {
                if let Some(v) = out.remove(key) {
                    out.insert(t, v);
                }
            }
        }
    }
    let keys: Vec<String> = out.keys().cloned().collect();
    for key in keys {
        let Some(field) = fields.get(&key) else { continue };
        let value = out[&key].clone();
        if value.is_null() && !required.contains(&key.as_str()) {
            out.remove(&key); // "limit": null means "no limit", which is what leaving it out says
            continue;
        }
        let ks = kinds(field);
        let mut value = value;
        if let Value::String(s) = &value {
            if !ks.iter().any(|k| k == "string") && matches!(s.trim().chars().next(), Some('[') | Some('{')) {
                if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                    value = parsed.clone();
                    out.insert(key.clone(), parsed);
                } else {
                    continue;
                }
            }
        }
        if value.is_object() && ks == ["array"] {
            out.insert(key, Value::Array(vec![value])); // one edit sent on its own instead of a list of one
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_and_args() {
        let offered: HashSet<String> = ["read_file", "edit", "shell"].iter().map(|s| s.to_string()).collect();
        assert_eq!(fix_tool_name("Read", &offered), "read_file");
        assert_eq!(fix_tool_name("bash", &offered), "shell");
        assert_eq!(fix_tool_name("nope", &offered), "nope");
        let schema = json!({"properties": {"path": {"type": "string"}, "old_string": {"type": "string"}, "new_string": {"type": "string"}, "edits": {"type": ["array", "null"]}, "limit": {"type": "integer"}}, "required": ["path"]});
        let args = json!({"file_path": "a.py", "old_str": "x", "newString": "y", "edits": "[{\"old_string\": \"a\"}]", "limit": null});
        let fixed = fix_arguments(&schema, args.as_object().unwrap());
        assert_eq!(fixed["path"], "a.py");
        assert_eq!(fixed["old_string"], "x");
        assert_eq!(fixed["new_string"], "y");
        assert!(fixed["edits"].is_array());
        assert!(!fixed.contains_key("limit"));
        let one = json!({"path": "a", "edits": {"old_string": "a"}});
        assert!(fix_arguments(&schema, one.as_object().unwrap())["edits"].is_array());
    }
}
