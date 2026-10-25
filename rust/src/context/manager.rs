use crate::client::response::TokenUsage;
use crate::config::ConfigHandle;
use crate::util::archive::ResultStore;
use crate::util::text::estimate_tokens;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct MessageItem {
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_calls: Vec<Value>,
    pub token_count: usize,
    pub pruned: bool,
    /// Thinking text, sent back to the same model while its tool loop runs.
    pub reasoning: String,
    pub reasoning_model: String,
}

impl MessageItem {
    pub fn new(role: &str, content: &str) -> MessageItem {
        MessageItem {
            role: role.into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            token_count: estimate_tokens(content),
            pruned: false,
            reasoning: String::new(),
            reasoning_model: String::new(),
        }
    }

    pub fn to_value(&self) -> Value {
        let mut m = json!({"role": self.role});
        if let Some(id) = &self.tool_call_id {
            m["tool_call_id"] = json!(id);
        }
        if !self.tool_calls.is_empty() {
            m["tool_calls"] = json!(self.tool_calls);
        }
        // providers reject a tool message with no content (e.g. `touch x` prints nothing), so those always carry text;
        // assistant tool-call turns send ""
        if !self.content.is_empty() {
            m["content"] = json!(self.content);
        } else if self.role == "tool" {
            m["content"] = json!("(no output)");
        } else if self.role == "assistant" {
            m["content"] = json!("");
        }
        if !self.reasoning.is_empty() {
            // the adapter keeps it only for the model that wrote it
            m["reasoning_content"] = json!(self.reasoning);
            m["reasoning_model"] = json!(self.reasoning_model);
        }
        m
    }

    pub fn from_value(v: &Value) -> MessageItem {
        let content = v["content"].as_str().unwrap_or("").to_string();
        let tool_calls: Vec<Value> = v["tool_calls"].as_array().cloned().unwrap_or_default();
        let reasoning = v["reasoning_content"].as_str().unwrap_or("").to_string();
        MessageItem {
            role: v["role"].as_str().unwrap_or("user").into(),
            token_count: estimate_tokens(&format!("{content}{reasoning}{}", if tool_calls.is_empty() { String::new() } else { Value::Array(tool_calls.clone()).to_string() })),
            content,
            tool_call_id: v["tool_call_id"].as_str().map(String::from),
            tool_calls,
            pruned: false,
            reasoning,
            reasoning_model: v["reasoning_model"].as_str().unwrap_or("").into(),
        }
    }
}

pub const COMPACT_HEAD: &str = "# Earlier conversation (compacted)";
const PRUNE_PROTECT_TOKENS: usize = 20_000;
const PRUNE_MINIMUM_TOKENS: usize = 20_000;

#[derive(Default, Clone, Debug)]
pub struct PruneInfo {
    pub results: usize,
    pub tokens_cleared: usize,
    pub prompt_tokens: i64,
    pub read_paths: Vec<String>,
}

pub struct ContextManager {
    system_prompt: String,
    config: ConfigHandle,
    pub messages: Vec<MessageItem>,
    pub latest_usage: TokenUsage,
    pub total_usage: TokenUsage,
    pub context_window: usize,
    pub summary: Option<String>,
    pub files_read: BTreeSet<String>,
    pub files_modified: BTreeSet<String>,
    pub archive: Option<Arc<ResultStore>>,
    pub last_prune: PruneInfo,
}

impl ContextManager {
    pub fn new(config: ConfigHandle, system_prompt: String) -> ContextManager {
        let window = config.get().model.context_window;
        ContextManager {
            system_prompt,
            config,
            messages: vec![],
            latest_usage: TokenUsage::default(),
            total_usage: TokenUsage::default(),
            context_window: window,
            summary: None,
            files_read: BTreeSet::new(),
            files_modified: BTreeSet::new(),
            archive: None,
            last_prune: PruneInfo::default(),
        }
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// A resumed session replays its messages, so the compacted head arrives as plain text. Take the summary and the
    /// file lists back out of it, otherwise the next compaction would forget which files were edited.
    fn restore_summary(&mut self, content: &str) {
        let parts: Vec<&str> = content.splitn(3, "\n\n").collect();
        let summary = if parts.len() == 3 { parts[2].to_string() } else { content.to_string() };
        for line in summary.lines() {
            if let Some(rest) = line.strip_prefix("Files modified so far: ") {
                self.files_modified.extend(rest.split(',').map(|p| p.trim().to_string()));
            } else if let Some(rest) = line.strip_prefix("Files read so far: ") {
                self.files_read.extend(rest.split(',').map(|p| p.trim().to_string()));
            }
        }
        self.summary = Some(summary);
    }

    pub fn add_user_message(&mut self, content: &str) {
        if self.messages.is_empty() && content.starts_with(COMPACT_HEAD) {
            self.restore_summary(content);
        }
        self.messages.push(MessageItem::new("user", content));
    }

    pub fn add_assistant_message(&mut self, content: Option<&str>, tool_calls: Option<Vec<Value>>, reasoning: Option<&str>, model: Option<&str>) {
        let mut item = MessageItem::new("assistant", content.unwrap_or(""));
        let calls = tool_calls.unwrap_or_default();
        // the call arguments count too: an edit or write_file carrying 5k of code is not a 0-token message
        item.token_count = estimate_tokens(&format!(
            "{}{}{}",
            content.unwrap_or(""),
            reasoning.unwrap_or(""),
            if calls.is_empty() { String::new() } else { Value::Array(calls.clone()).to_string() }
        ));
        item.tool_calls = calls;
        if let Some(r) = reasoning.filter(|r| !r.is_empty()) {
            item.reasoning = r.into();
            item.reasoning_model = model.unwrap_or("").into();
        }
        self.messages.push(item);
    }

    pub fn add_tool_result(&mut self, tool_call_id: &str, content: &str) {
        let mut item = MessageItem::new("tool", content);
        item.tool_call_id = Some(tool_call_id.into());
        self.messages.push(item);
    }

    pub fn get_messages(&self) -> Vec<Value> {
        let mut out = Vec::with_capacity(self.messages.len() + 1);
        if !self.system_prompt.is_empty() {
            out.push(json!({"role": "system", "content": self.system_prompt}));
        }
        out.extend(self.messages.iter().map(MessageItem::to_value));
        out
    }

    pub fn limit(&self) -> f64 {
        let cfg = self.config.get();
        if cfg.pi_mode {
            return self.context_window as f64 - 16_384.0; // pi: compact only when the next reply would no longer fit
        }
        // a 1M window doesn't mean 800k tokens of history is a good idea: cap with compact_at_tokens
        (self.context_window as f64 * 0.8).min(cfg.compact_at_tokens as f64)
    }

    /// What the next prompt will roughly weigh, before the provider says. Right after a compaction this is the only
    /// honest number: the last usage on record is the summarizer's own call.
    pub fn estimated_tokens(&self) -> usize {
        estimate_tokens(&self.system_prompt) + self.messages.iter().map(|m| m.token_count).sum::<usize>()
    }

    pub fn context_used(&self) -> usize {
        if self.latest_usage.total_tokens > 0 { self.latest_usage.total_tokens as usize } else { self.estimated_tokens() }
    }

    pub fn needs_compression(&self) -> bool {
        self.latest_usage.total_tokens as f64 > self.limit()
    }

    pub fn set_latest_usage(&mut self, u: TokenUsage) {
        self.latest_usage = u;
    }

    pub fn add_usage(&mut self, u: &TokenUsage) {
        self.total_usage.add(u);
    }

    /// Index where the kept tail starts. The tail holds roughly `keep_tokens` of the most recent messages and never
    /// starts on a tool result (its assistant tool_calls message must travel with it).
    pub fn find_cut(&self, keep_tokens: usize) -> usize {
        let (mut total, mut cut) = (0usize, self.messages.len());
        for i in (0..self.messages.len()).rev() {
            total += self.messages[i].token_count;
            cut = i;
            if total >= keep_tokens {
                break;
            }
        }
        while cut > 0 && cut < self.messages.len() && self.messages[cut].role == "tool" {
            cut -= 1;
        }
        cut
    }

    pub fn history_for_summary(&self, cut: usize) -> Vec<Value> {
        // the compacted head is passed whole as the previous summary, not as a message to fold in
        let skip = usize::from(self.summary.is_some() && self.messages.first().map_or(false, |m| m.content.starts_with(COMPACT_HEAD)));
        self.messages[skip.min(cut)..cut].iter().map(MessageItem::to_value).collect()
    }

    fn track_files(&mut self, upto: usize) {
        for m in &self.messages[..upto] {
            for tc in &m.tool_calls {
                let args: Option<Value> = tc["function"]["arguments"].as_str().and_then(|a| serde_json::from_str(a).ok());
                let Some(path) = args.as_ref().and_then(|a| a["path"].as_str()) else { continue };
                match tc["function"]["name"].as_str().unwrap_or("") {
                    "read_file" => {
                        self.files_read.insert(path.into());
                    }
                    "edit" | "write_file" => {
                        self.files_modified.insert(path.into());
                    }
                    _ => {}
                }
            }
        }
    }

    /// Files read or edited in the part about to be compacted, newest first, skipping any the kept tail touches again.
    pub fn recent_files(&self, cut: usize, limit: usize) -> Vec<String> {
        fn paths(items: &[MessageItem]) -> Vec<String> {
            let mut out = vec![];
            for m in items {
                for tc in &m.tool_calls {
                    if !matches!(tc["function"]["name"].as_str(), Some("read_file" | "edit" | "write_file")) {
                        continue;
                    }
                    let p = tc["function"]["arguments"].as_str().and_then(|a| serde_json::from_str::<Value>(a).ok()).and_then(|a| a["path"].as_str().map(String::from));
                    out.extend(p);
                }
            }
            out
        }
        let in_tail: std::collections::HashSet<String> = paths(&self.messages[cut..]).into_iter().collect();
        let mut picked: Vec<String> = vec![];
        for p in paths(&self.messages[..cut]).into_iter().rev() {
            if !in_tail.contains(&p) && !picked.contains(&p) {
                picked.push(p);
            }
        }
        picked.truncate(limit);
        picked
    }

    /// Replace messages[..cut] with the summary and keep the recent tail verbatim. With `transcript`, the replaced
    /// messages are appended there first (one JSON per line), so nothing is gone for good.
    pub fn apply_compaction(&mut self, summary: &str, cut: usize, transcript: Option<&Path>, notes: Option<String>) {
        self.track_files(cut);
        let mut saved = String::new();
        if let Some(t) = transcript {
            if let Some(p) = t.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            let wrote = (|| -> std::io::Result<()> {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().create(true).append(true).open(t)?;
                for m in self.history_for_summary(cut) {
                    writeln!(f, "{m}")?;
                }
                Ok(())
            })();
            if wrote.is_ok() {
                saved = format!(
                    " If you need exact details from before compaction (a code snippet, an error, a list the user pasted), grep or read the full transcript at {}.",
                    t.display()
                );
            }
        }
        let mut files = String::new();
        if !self.files_modified.is_empty() {
            files.push_str(&format!("\n\nFiles modified so far: {}", self.files_modified.iter().cloned().collect::<Vec<_>>().join(", ")));
        }
        let reads: Vec<&String> = self.files_read.difference(&self.files_modified).collect();
        if !reads.is_empty() {
            files.push_str(&format!("\nFiles read so far: {}", reads.iter().take(40).map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
        }
        if let Some(n) = notes.filter(|n| !n.is_empty()) {
            files.push_str(&format!("\n\nTask notes (kept with the notes tool, still current):\n{}", crate::util::text::head_chars(&n, 1500)));
        }
        self.summary = Some(format!("{summary}{files}"));
        let content = format!(
            "{COMPACT_HEAD}\n\nOlder messages were replaced by this summary to save context. Actions listed as completed are done; do not repeat them. The most recent messages follow unchanged.{saved}\n\n{}",
            self.summary.as_ref().unwrap()
        );
        let head = MessageItem::new("user", &content);
        let tail: Vec<MessageItem> = self.messages.split_off(cut);
        self.messages = std::iter::once(head).chain(tail).collect();
    }

    /// True while that call's result is still in the history with its full text.
    pub fn has_live_result(&self, call_id: &str) -> bool {
        self.messages.iter().any(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(call_id) && !m.pruned)
    }

    /// After an interrupt, an assistant message may have tool_calls with no results. Providers reject that, so close
    /// each one with a stub result.
    pub fn repair_orphans(&mut self) -> usize {
        let mut fixed = 0;
        let mut out: Vec<MessageItem> = vec![];
        let msgs = std::mem::take(&mut self.messages);
        let mut i = 0;
        while i < msgs.len() {
            let m = &msgs[i];
            out.push(m.clone());
            i += 1;
            if m.role == "assistant" && !m.tool_calls.is_empty() {
                let mut answered = vec![];
                while i < msgs.len() && msgs[i].role == "tool" {
                    answered.push(msgs[i].tool_call_id.clone().unwrap_or_default());
                    out.push(msgs[i].clone());
                    i += 1;
                }
                for tc in &m.tool_calls {
                    let id = tc["id"].as_str().unwrap_or("").to_string();
                    if !answered.contains(&id) {
                        let mut stub = MessageItem::new("tool", "[interrupted before this tool ran]");
                        stub.tool_call_id = Some(id);
                        stub.token_count = 8;
                        out.push(stub);
                        fixed += 1;
                    }
                }
            }
        }
        self.messages = out;
        fixed
    }

    /// Clear old tool results beyond the newest ~20k tokens. Cleared results are archived (outside pi mode) so they can
    /// be recalled exactly.
    pub fn prune_tool_outputs(&mut self) -> usize {
        let (mut total, mut pruned_tokens) = (0usize, 0usize);
        let mut to_prune: Vec<usize> = vec![];
        for idx in (0..self.messages.len()).rev() {
            let m = &self.messages[idx];
            if m.role == "tool" && m.tool_call_id.is_some() {
                if m.pruned {
                    break;
                }
                total += m.token_count;
                if total > PRUNE_PROTECT_TOKENS {
                    pruned_tokens += m.token_count;
                    to_prune.push(idx);
                }
            }
        }
        if pruned_tokens < PRUNE_MINIMUM_TOKENS {
            return 0;
        }
        let mut by_call: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        for m in &self.messages {
            for tc in &m.tool_calls {
                if let Some(id) = tc["id"].as_str() {
                    by_call.insert(id.into(), tc["function"].clone());
                }
            }
        }
        let pi = self.config.get().pi_mode;
        let mut paths = BTreeSet::new();
        let mut count = 0;
        for idx in to_prune {
            let fname = self.messages[idx].tool_call_id.as_ref().and_then(|id| by_call.get(id)).cloned();
            if let Some(f) = &fname {
                if f["name"] == "read_file" {
                    if let Some(p) = f["arguments"].as_str().and_then(|a| serde_json::from_str::<Value>(a).ok()).and_then(|a| a["path"].as_str().map(String::from)) {
                        paths.insert(p);
                    }
                }
            }
            let m = &mut self.messages[idx];
            let head = m.content.trim().lines().next().map(|l| crate::util::text::head_chars(l, 100));
            let mut hint = head.map(|h| format!(": {h}")).unwrap_or_default();
            // name the call that made it, so a model that needs it again re-runs exactly that
            if let Some(f) = &fname {
                let args = f["arguments"].as_str().unwrap_or("");
                hint = format!(" from {}({}){}", f["name"].as_str().unwrap_or("tool"), crate::util::text::head_chars(args, 120), hint);
            }
            m.content = match (&self.archive, pi) {
                (Some(a), false) => {
                    let rid = a.put(&m.content);
                    format!("[Old tool result cleared to save tokens ({rid}, {} chars){hint}. recall_result {rid} returns the exact text.]", m.content.chars().count())
                }
                _ => format!("[Old tool result cleared to save tokens{hint}. Re-run the tool if you need it again.]"),
            };
            m.token_count = estimate_tokens(&m.content);
            m.pruned = true;
            count += 1;
        }
        self.last_prune = PruneInfo { results: count, tokens_cleared: pruned_tokens, prompt_tokens: self.latest_usage.prompt_tokens, read_paths: paths.into_iter().collect() };
        count
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.summary = None;
        self.files_read.clear();
        self.files_modified.clear();
        self.latest_usage = TokenUsage::default(); // the old prompt size would keep showing in the footer
        self.last_prune = PruneInfo::default();
    }
}
