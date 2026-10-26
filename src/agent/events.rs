use crate::tools::ToolResult;
use serde_json::{json, Map, Value};

/// What the front end hears while a request runs.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    AgentStart { message: String },
    AgentEnd { response: Option<String>, report: Value },
    AgentError { error: String },
    TextDelta(String),
    TextComplete(String),
    /// `tokens` is the new work spent so far in this request (sub-agents report it as progress).
    ToolCallStart { call_id: String, name: String, arguments: Map<String, Value>, tokens: i64 },
    ToolCallComplete { call_id: String, name: String, result: ToolResult },
    /// Harness decisions worth showing (e.g. model fallback).
    Notice(String),
}

impl AgentEvent {
    pub fn notice(m: impl Into<String>) -> AgentEvent {
        AgentEvent::Notice(m.into())
    }

    pub fn to_value(&self) -> Value {
        match self {
            AgentEvent::AgentStart { message } => json!({"type": "agent_start", "message": message}),
            AgentEvent::AgentEnd { response, report } => json!({"type": "agent_end", "response": response, "report": report}),
            AgentEvent::AgentError { error } => json!({"type": "agent_error", "error": error}),
            AgentEvent::TextDelta(c) => json!({"type": "text_delta", "content": c}),
            AgentEvent::TextComplete(c) => json!({"type": "text_complete", "content": c}),
            AgentEvent::ToolCallStart { call_id, name, arguments, .. } => json!({"type": "tool_call_start", "call_id": call_id, "name": name, "arguments": arguments}),
            AgentEvent::ToolCallComplete { call_id, name, result } => json!({
                "type": "tool_call_complete", "call_id": call_id, "name": name, "success": result.success, "output": result.output,
                "error": result.error, "metadata": result.metadata, "diff": result.diff.as_ref().map(|d| d.to_diff()),
                "truncated": result.truncated, "exit_code": result.exit_code}),
            AgentEvent::Notice(m) => json!({"type": "notice", "message": m}),
        }
    }
}
