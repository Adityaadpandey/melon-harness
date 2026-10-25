//! Ported from tests/test_tool_repair.py: near-miss tool calls from open models are fixed, not bounced back.
use crate::client::response::parse_tool_call_arguments as parse;
use crate::tools::builtin::{edit_file::EditTool, read_file::ReadFileTool};
use crate::tools::repair::{fix_arguments, fix_tool_name};
use crate::tools::Tool;
use serde_json::{json, Value};
use std::collections::HashSet;

fn offered() -> HashSet<String> {
    ["read_file", "write_file", "edit", "shell", "grep", "glob", "list_dir", "todos"].iter().map(|s| s.to_string()).collect()
}

fn fix(tool: &dyn Tool, args: Value) -> Value {
    Value::Object(fix_arguments(&tool.schema(), args.as_object().unwrap()))
}

#[test]
fn almost_json_is_repaired() {
    let obj = |v: Value| v.as_object().unwrap().clone();
    // lone backslash from a regex
    assert_eq!(parse(r#"{"pattern": "\d+"}"#), obj(json!({"pattern": "\\d+"})));
    // raw newline inside a string
    assert_eq!(parse("{\"content\": \"a\nb\"}"), obj(json!({"content": "a\nb"})));
    assert_eq!(parse("```json\n{\"path\": \"a\"}\n```"), obj(json!({"path": "a"})));
    // encoded twice
    assert_eq!(parse(r#""{\"path\": \"a\"}""#), obj(json!({"path": "a"})));
    // truncated: left for the model to re-send
    assert_eq!(parse(r#"{"path": "a"#), obj(json!({"raw_arguments": r#"{"path": "a"#})));
}

#[test]
fn tool_names_from_other_harnesses_map_to_ours() {
    let o = offered();
    assert_eq!(fix_tool_name("Read", &o), "read_file");
    assert_eq!(fix_tool_name("Bash", &o), "shell");
    assert_eq!(fix_tool_name("str_replace", &o), "edit");
    assert_eq!(fix_tool_name("GREP", &o), "grep");
    // not offered: unchanged, the error lists the real tools
    assert_eq!(fix_tool_name("web_search", &o), "web_search");
}

#[test]
fn argument_slips_are_fixed_only_where_the_schema_allows_one_reading() {
    let edit = EditTool;
    assert_eq!(fix(&edit, json!({"file_path": "a.py", "old_str": "x", "new_str": "y"})), json!({"path": "a.py", "old_string": "x", "new_string": "y"}));
    assert_eq!(fix(&edit, json!({"filePath": "a.py", "oldString": "x", "newString": "y", "replaceAll": true}))["replace_all"], json!(true));
    // GLM sends the edits list as a JSON string; others send one edit without the list
    let as_text = fix(&edit, json!({"path": "a", "edits": "[{\"old_string\": \"x\", \"new_string\": \"y\"}]"}));
    assert_eq!(as_text["edits"], json!([{"old_string": "x", "new_string": "y"}]));
    let single = fix(&edit, json!({"path": "a", "edits": {"old_string": "x", "new_string": "y"}}));
    assert_eq!(single["edits"], json!([{"old_string": "x", "new_string": "y"}]));
    // the repaired call passes the tool's own validation
    assert!(edit.validate_params(&single).is_ok());
    let read = ReadFileTool::new(super::handle(std::path::Path::new("/tmp")));
    assert_eq!(fix(&read, json!({"path": "a", "limit": null})), json!({"path": "a"}));
    // a string field that happens to hold JSON is left alone
    assert_eq!(fix(&edit, json!({"path": "a", "old_string": "{}", "new_string": "[1]"}))["new_string"], json!("[1]"));
}
