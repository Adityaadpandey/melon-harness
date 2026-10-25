//! The advisor: a second, stronger model that reads the whole conversation and says what the executor is about to get
//! wrong. It takes no parameters; calling it forwards everything the agent has seen and done.
use crate::client::llm_client::LLMClient;
use crate::client::response::StreamEvent;
use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use std::any::Any;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const ADVISOR_SYSTEM: &str = "You are the advisor: a senior engineer reading over the shoulder of a coding agent. You see its whole transcript (the task,
every tool call, every result). You cannot run anything; you can only think, so think hard about the logic.

Answer in under 250 words, numbered, most important first:
- Is the agent solving the right problem? Name the root cause if it is fixing a symptom (a guard in one caller while every caller shares the broken function).
- What does the planned or finished change miss: other callers, the mirrored half (desktop/mobile, create/update, server/client), empty/null/error paths, ordering and races, old data and old clients?
- If it believes it is done: which concrete input would still break it? What would you run to prove it (a command or a test), not just read?
- If evidence in the transcript contradicts the plan, say so plainly and say which side wins.
No praise, no restating the task, no generic advice. If the plan is sound, say so in one line and name the one thing to double-check.";

const TRANSCRIPT_CAP: usize = 60_000;

fn per_message(role: &str) -> usize {
    match role {
        "user" => 5000,
        "assistant" => 3500,
        _ => 1500,
    }
}

/// The conversation as plain text. The request that started it is always kept; when the rest is too long the oldest
/// middle goes.
pub fn render_transcript(messages: &[Value], cap: usize) -> String {
    let mut rows: Vec<String> = vec![];
    for m in messages {
        let role = m["role"].as_str().unwrap_or("");
        if role == "system" {
            continue;
        }
        let text = m["content"].as_str().unwrap_or("");
        let limit = per_message(role);
        let cut: String = text.chars().take(limit).collect();
        let mut row = format!("[{role}] {cut}{}", if text.chars().count() > limit { " ...[cut]" } else { "" });
        for call in m["tool_calls"].as_array().into_iter().flatten() {
            let f = &call["function"];
            let args = f["arguments"].as_str().unwrap_or("");
            row.push_str(&format!("\n  -> {}({})", f["name"].as_str().unwrap_or(""), crate::util::text::head_chars(&Value::String(args.into()).to_string(), 400)));
        }
        rows.push(row);
    }
    if rows.is_empty() {
        return String::new();
    }
    let head = rows.remove(0);
    let (mut out, mut used) = (vec![], 0usize);
    for row in rows.iter().rev() {
        if used + row.chars().count() > cap {
            out.push("[... earlier steps left out ...]".to_string());
            break;
        }
        used += row.chars().count();
        out.push(row.clone());
    }
    out.reverse();
    std::iter::once(head).chain(out).collect::<Vec<_>>().join("\n\n")
}

pub type TranscriptFn = Arc<dyn Fn() -> Vec<Value> + Send + Sync>;

pub struct AdvisorTool {
    config: ConfigHandle,
    pub client: Mutex<Option<Arc<LLMClient>>>,
    pub transcript: Mutex<Option<TranscriptFn>>,
    /// Advisor calls in this request; the agent resets it per request.
    pub calls: AtomicUsize,
    /// The registry's write count when it was last called, so "consulted after the last edit" can be checked.
    pub last_call_at: AtomicI64,
}

impl AdvisorTool {
    pub fn new(config: ConfigHandle) -> Self {
        AdvisorTool { config, client: Mutex::new(None), transcript: Mutex::new(None), calls: AtomicUsize::new(0), last_call_at: AtomicI64::new(-1) }
    }
}

#[async_trait]
impl Tool for AdvisorTool {
    fn name(&self) -> &str {
        "advisor"
    }
    fn description(&self) -> String {
        "Ask a stronger reviewer model for advice. Takes no parameters: your whole conversation is forwarded. Call it BEFORE you edit (after reading the code), when you think you are done (after saving the work), when stuck, or before changing approach. Skip it for trivial one-step tasks.".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> Value {
        schema_of(json!({}), &[])
    }
    async fn execute(&self, _inv: &ToolInvocation) -> ToolResult {
        let (client, transcript) = (self.client.lock().unwrap().clone(), self.transcript.lock().unwrap().clone());
        let (Some(client), Some(transcript)) = (client, transcript) else { return ToolResult::error("The advisor is not connected in this run. Carry on without it.") };
        let max = self.config.get().advisor_max_calls;
        if self.calls.load(Ordering::SeqCst) >= max {
            return ToolResult::error(format!("Advisor limit reached ({max} calls this request). Decide with what you have."));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        let prompt = vec![
            json!({"role": "system", "content": ADVISOR_SYSTEM}),
            json!({"role": "user", "content": format!("TRANSCRIPT SO FAR:\n\n{}\n\nGive your advice now.", render_transcript(&transcript(), TRANSCRIPT_CAP))}),
        ];
        let (mut advice, mut error) = (String::new(), String::new());
        let mut stream = client.chat_completion(prompt, None, false, "advisor");
        while let Some(ev) = stream.next().await {
            match ev {
                StreamEvent::TextDelta(t) => advice.push_str(&t),
                StreamEvent::MessageComplete { text: Some(t), .. } if advice.is_empty() => advice = t, // a non-streaming reply arrives whole on the last event
                StreamEvent::Error(e) => error = e,
                _ => {}
            }
        }
        if advice.trim().is_empty() {
            return ToolResult::error(format!("The advisor could not answer ({}). Carry on without it.", if error.is_empty() { "empty reply".to_string() } else { crate::util::text::head_chars(&error, 160) }));
        }
        ToolResult::ok(format!("ADVISOR:\n{}", advice.trim()))
    }
}
