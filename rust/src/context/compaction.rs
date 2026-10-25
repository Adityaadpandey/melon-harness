//! Summarising the older part of the history so a long session fits the window again.
use super::manager::ContextManager;
use crate::client::llm_client::LLMClient;
use crate::client::response::{StreamEvent, TokenUsage};
use crate::prompts::get_compression_prompt;
use crate::util::precompact::pre_compact;
use futures::StreamExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// A reply with fewer than three of these is chatter, not the handoff the next turns depend on.
const HEADINGS: &[&str] = &["## ORIGINAL GOAL", "## COMPLETED ACTIONS", "## CURRENT STATE", "## NEXT STEP", "## USER MESSAGES", "## KEY CONTEXT"];

/// Sandwiched like Claude Code's: open models given tools mid-conversation reach for them unless told twice.
fn fork_prompt() -> String {
    format!(
        "CRITICAL: Respond with TEXT ONLY. Do not call any tools; a tool call here fails the task.\n\nThe conversation above is about to be replaced by your summary; the newest messages, which you cannot see, are kept word for word after it. If it opens with an earlier summary, carry its facts forward.\n\n{}\n\nREMINDER: plain text with the headings above, no tool calls.",
        get_compression_prompt()
    )
}

pub struct ChatCompactor {
    client: Arc<LLMClient>,
}

fn format_history(messages: &[Value]) -> String {
    let mut output = vec!["Here is the conversation that needs to be continue: \n".to_string()];
    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("");
        let content = msg["content"].as_str().unwrap_or("");
        match role {
            "system" => {}
            "tool" => {
                let id = msg["tool_call_id"].as_str().unwrap_or("unknown");
                let content = pre_compact(content); // drop progress bars and repeated lines so the 2000 chars hold real output
                let cut: String = content.chars().take(2000).collect();
                let more = if content.chars().count() > 2000 { "\n... [tool output truncated]" } else { "" };
                output.push(format!("[Tool Result ({id})]:\n{cut}{more}"));
            }
            "assistant" => {
                if !content.is_empty() {
                    let cut: String = content.chars().take(3000).collect();
                    output.push(format!("Assistant:\n{cut}{}", if content.chars().count() > 3000 { "\n... [response truncated]" } else { "" }));
                }
                if let Some(calls) = msg["tool_calls"].as_array().filter(|c| !c.is_empty()) {
                    let details: Vec<String> = calls
                        .iter()
                        .map(|tc| {
                            let f = &tc["function"];
                            // todos/notes args carry the plan itself; 500 chars cut a 12-step list after step 4
                            format!("  - {}({})", f["name"].as_str().unwrap_or("unknown"), f["arguments"].as_str().unwrap_or("{}").chars().take(2000).collect::<String>())
                        })
                        .collect();
                    output.push(format!("Assistant called tools:\n{}", details.join("\n")));
                }
            }
            _ => {
                // user messages go in whole: they are the task. ponytail: a 20k cap still guards against a pasted log
                let c = if content.chars().count() > 20_000 { format!("{}\n... [message truncated]", content.chars().take(20_000).collect::<String>()) } else { content.to_string() };
                output.push(format!("User:\n{c}"));
            }
        }
    }
    output.join("\n\n---\n\n")
}

impl ChatCompactor {
    pub fn new(client: Arc<LLMClient>) -> Self {
        ChatCompactor { client }
    }

    /// One summary request: (text, usage, ok). Not ok when it failed, stopped at the output limit (the last sections are
    /// gone), tried to call a tool, or came back without the handoff sections.
    async fn ask(&self, messages: Vec<Value>, tools: Option<Vec<Value>>, role: &str) -> (String, Option<TokenUsage>, bool) {
        let (mut summary, mut usage, mut failed) = (String::new(), None, false);
        let mut s = self.client.chat_completion(messages, tools, false, role);
        while let Some(ev) = s.next().await {
            match ev {
                StreamEvent::MessageComplete { text, usage: u, finish_reason, .. } => {
                    usage = u;
                    summary.push_str(&text.unwrap_or_default());
                    failed |= matches!(finish_reason.as_deref(), Some("length" | "max_tokens" | "max_output_tokens"));
                }
                StreamEvent::ToolCallComplete(_) => failed = true, // it tried to act instead of summarising
                StreamEvent::Error(_) => failed = true,
                _ => {}
            }
        }
        let ok = !summary.is_empty() && !failed && HEADINGS.iter().filter(|h| summary.contains(*h)).count() >= 3;
        (summary, usage, ok)
    }

    /// Summarise the older part of the history (together with any earlier summary), leaving the recent tail untouched.
    /// Returns (summary, usage, cut). With `fork` = (tool schemas, role), the summary is first asked the way Claude Code
    /// asks it: the exact system prompt, tools and older messages the agent loop sent, plus one instruction at the end.
    /// That prefix is already in the provider's cache. If that fails, the trimmed copy below is the fallback.
    pub async fn compress(&self, cm: &Mutex<ContextManager>, keep_recent_tokens: usize, fork: Option<(Vec<Value>, String)>) -> (Option<String>, Option<TokenUsage>, usize) {
        let (cut, prefix, mut msgs, prev_summary) = {
            let cm = cm.lock().unwrap();
            let mut cut = cm.find_cut(keep_recent_tokens);
            if cut == 0 {
                cut = cm.find_cut((keep_recent_tokens / 4).max(1)); // the whole history is "recent": fall back to keeping a quarter
            }
            if cut == 0 {
                return (None, None, 0);
            }
            let all = cm.get_messages();
            let prefix: Vec<Value> = all.into_iter().take(cut + 1).collect(); // the system prompt, then messages[..cut] exactly as the loop sent them
            (cut, prefix, cm.history_for_summary(cut), cm.summary.clone())
        };

        if let Some((tools, role)) = fork {
            let mut m = prefix;
            m.push(json!({"role": "user", "content": fork_prompt()}));
            let (summary, usage, ok) = self.ask(m, if tools.is_empty() { None } else { Some(tools) }, &role).await;
            if ok {
                return (Some(summary), usage, cut);
            }
        }

        // Claude Code's trick when the summary request is itself too long: drop the oldest fifth of the messages and ask
        // again (3 tries). Losing the far past beats losing the whole compaction.
        for _attempt in 0..4 {
            let mut history = format_history(&msgs);
            if let Some(s) = &prev_summary {
                history = format!("PREVIOUS SUMMARY (update it, do not drop facts that still matter):\n{s}\n\n---\n\nNEW MESSAGES TO FOLD IN:\n{history}");
            }
            let m = vec![json!({"role": "system", "content": get_compression_prompt()}), json!({"role": "user", "content": history})];
            let (summary, usage, ok) = self.ask(m, None, "summarize").await;
            if ok {
                return (Some(summary), usage, cut);
            }
            if msgs.len() < 5 {
                break;
            }
            let mut drop = msgs.len() / 5;
            while drop < msgs.len() && msgs[drop]["role"] == "tool" {
                drop += 1; // a tool result must not lose the assistant call that made it
            }
            msgs.drain(..drop.min(msgs.len()));
        }
        (None, None, 0)
    }
}
