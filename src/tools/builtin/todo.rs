use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::sync::Mutex;

#[derive(Clone, Debug)]
pub struct Todo {
    pub id: String,
    pub content: String,
    pub done: bool,
}

#[derive(Default)]
pub struct TodosTool {
    todos: Mutex<Vec<Todo>>, // creation order
}

impl TodosTool {
    /// (content, done) in creation order, for the live checklist in the UI.
    pub fn snapshot(&self) -> Vec<(String, bool)> {
        self.todos.lock().unwrap().iter().map(|t| (t.content.clone(), t.done)).collect()
    }

    /// The list as plain text, so it can be put back into the conversation after old results are cleared.
    pub fn render(&self) -> String {
        self.todos.lock().unwrap().iter().map(|t| format!("[{}] {}", if t.done { 'x' } else { ' ' }, t.content)).collect::<Vec<_>>().join("\n")
    }

    pub fn unfinished(&self) -> Vec<String> {
        self.todos.lock().unwrap().iter().filter(|t| !t.done).map(|t| t.content.clone()).collect()
    }

    pub fn replace(&self, items: Vec<Todo>) {
        *self.todos.lock().unwrap() = items;
    }

    pub fn items(&self) -> Vec<Todo> {
        self.todos.lock().unwrap().clone()
    }

    /// An id, or failing that the step's text: complete "inspect" works when the step reads "Inspect the nav links".
    fn find(todos: &[Todo], r: &str) -> Option<usize> {
        if let Some(i) = todos.iter().position(|t| t.id == r) {
            return Some(i);
        }
        let hits: Vec<usize> = todos.iter().enumerate().filter(|(_, t)| t.content.to_lowercase().contains(&r.to_lowercase())).map(|(i, _)| i).collect();
        (hits.len() == 1).then(|| hits[0])
    }
}

#[async_trait]
impl Tool for TodosTool {
    fn name(&self) -> &str {
        "todos"
    }
    fn description(&self) -> String {
        "Live checklist for long work: 5+ steps, a sweep over many files, or several requests. Skip it when a few tool calls finish the job. Prefer `set` with the whole list, and send each update in the same turn as your next real call (an edit, a test run), never in a turn of its own: every turn re-sends the whole conversation. Only mark a step done when it really passed: if tests fail or the work is partial, keep it open.".into()
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
                "action": {"type": "string", "description": "Action: 'set' (replace the whole list in one call), 'add', 'complete', 'list', 'clear'"},
                "id": {"type": ["string", "null"], "description": "Todo ID (for complete)"},
                "content": {"type": ["string", "null"], "description": "Todo content (for add)"},
                "items": {"type": ["array", "null"], "description": "The full list, for 'set': every step with done true/false. One call replaces many add/complete calls",
                          "items": {"type": "object", "properties": {"id": {"type": ["string", "null"]}, "content": {"type": "string"}, "done": {"type": "boolean"}}, "required": ["content"]}},
            }),
            &["action"],
        )
    }

    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let action = inv.str("action").unwrap_or("").to_string();
        let mut todos = self.todos.lock().unwrap();
        match action.to_lowercase().as_str() {
            "set" => {
                let Some(items) = inv.params["items"].as_array().filter(|a| !a.is_empty()) else { return ToolResult::error("`items` required for 'set' action") };
                let list: Vec<Todo> = items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| Todo {
                        id: it["id"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| (i + 1).to_string()),
                        content: it["content"].as_str().unwrap_or("").to_string(),
                        done: it["done"].as_bool().unwrap_or(false),
                    })
                    .collect();
                let done = list.iter().filter(|t| t.done).count();
                let n = list.len();
                *todos = list;
                ToolResult::ok(format!("Todo list set: {n} steps, {done} done"))
            }
            "add" => {
                let Some(c) = inv.str("content").filter(|c| !c.is_empty()) else { return ToolResult::error("`content` required for 'add' action") };
                let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
                todos.push(Todo { id: id.clone(), content: c.into(), done: false });
                ToolResult::ok(format!("Added todo [{id}]: {c}"))
            }
            "complete" => {
                let Some(id) = inv.str("id").filter(|c| !c.is_empty()) else { return ToolResult::error("`id` required for 'complete' action") };
                match Self::find(&todos, id) {
                    None => {
                        let ids: Vec<String> = todos.iter().map(|t| t.id.clone()).collect();
                        ToolResult::error(format!("Todo not found: {id}. Current ids: {}", if ids.is_empty() { "none".into() } else { ids.join(", ") }))
                    }
                    Some(i) => {
                        todos[i].done = true; // kept, so the checklist shows what is finished
                        ToolResult::ok(format!("Completed todo [{}]: {}", todos[i].id, todos[i].content))
                    }
                }
            }
            _ if action == "list" => {
                if todos.is_empty() {
                    return ToolResult::ok("No todos");
                }
                let mut lines = vec!["Todos:".to_string()];
                lines.extend(todos.iter().map(|t| format!("  [{}] {} {}", t.id, if t.done { "[x]" } else { "[ ]" }, t.content)));
                ToolResult::ok(lines.join("\n"))
            }
            _ if action == "clear" => {
                let n = todos.len();
                todos.clear();
                ToolResult::ok(format!("Cleared {n} todos"))
            }
            _ => ToolResult::error(format!("Unknown action: {action}")),
        }
    }
}
