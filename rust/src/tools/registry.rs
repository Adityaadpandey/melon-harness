//! Every tool call goes through here: repair, gates, validation, repository rules, hooks, approval, execution, output
//! caps, per-edit diagnostics and the folder instructions that ride on the first touch of a folder.
use super::*;
use crate::config::ConfigHandle;
use crate::hooks::HookSystem;
use crate::safety::approval::{ApprovalContext, ApprovalDecision, ApprovalManager};
use crate::safety::guard::violation;
use crate::util::archive::ResultStore;
use crate::util::paths::resolve_path;
use crate::util::trace::Tracer;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

pub const PI_MAX_LINES: usize = 2000;
pub const PI_MAX_BYTES: usize = 50 * 1024;
/// A 28k-char test log costs ~7k tokens on every later call; the tail has the summary, the rest is in the temp file.
pub const SHELL_MAX_BYTES: usize = 12 * 1024;

/// Read-only lookups whose answer only changes when something is written; read_file has its own finer dedup.
const REUSABLE: &[&str] = &["grep", "search", "glob", "list_dir", "outline", "git", "graph"];

pub type Gate = std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
type Stamp = Option<(u128, u64)>;

pub struct ToolRegistry {
    tools: Mutex<Vec<Arc<dyn Tool>>>,
    mcp_tools: Mutex<Vec<Arc<dyn Tool>>>,
    pub config: ConfigHandle,
    pub tracer: Mutex<Option<Arc<Tracer>>>,
    pub epoch: AtomicU64,
    /// Set by the agent during a fix run: tool name -> refusal text or None.
    pub gate: Mutex<Option<Gate>>,
    pub progress: Mutex<Option<ProgressFn>>,
    instructions_seen: Mutex<HashSet<PathBuf>>,
    read_state: Mutex<HashMap<String, Stamp>>,
    announced: Mutex<HashMap<String, Stamp>>,
    answered: Mutex<HashMap<String, (String, u64)>>,
    /// Bumps after every call that can change the workspace.
    pub writes: AtomicU64,
    is_live: Mutex<std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    pub archive: Mutex<Arc<ResultStore>>,
}

fn stamp(p: &Path) -> Stamp {
    let m = std::fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((t, m.len()))
}

impl ToolRegistry {
    pub fn new(config: ConfigHandle) -> ToolRegistry {
        ToolRegistry {
            tools: Mutex::new(vec![]),
            mcp_tools: Mutex::new(vec![]),
            config,
            tracer: Mutex::new(None),
            epoch: AtomicU64::new(0),
            gate: Mutex::new(None),
            progress: Mutex::new(None),
            instructions_seen: Mutex::new(HashSet::new()),
            read_state: Mutex::new(HashMap::new()),
            announced: Mutex::new(HashMap::new()),
            answered: Mutex::new(HashMap::new()),
            writes: AtomicU64::new(0),
            is_live: Mutex::new(Arc::new(|_| true)),
            archive: Mutex::new(Arc::new(ResultStore::default())),
        }
    }

    pub fn set_is_live(&self, f: Arc<dyn Fn(&str) -> bool + Send + Sync>) {
        *self.is_live.lock().unwrap() = f;
    }

    pub fn register(&self, tool: Arc<dyn Tool>) {
        let mut t = self.tools.lock().unwrap();
        if let Some(slot) = t.iter_mut().find(|x| x.name() == tool.name()) {
            *slot = tool;
        } else {
            t.push(tool);
        }
    }

    pub fn register_mcp_tool(&self, tool: Arc<dyn Tool>) {
        self.mcp_tools.lock().unwrap().push(tool);
    }

    pub fn unregister(&self, name: &str) -> bool {
        let mut t = self.tools.lock().unwrap();
        let before = t.len();
        t.retain(|x| x.name() != name);
        t.len() != before
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.lock().unwrap().iter().chain(self.mcp_tools.lock().unwrap().iter()).find(|t| t.name() == name).cloned()
    }

    pub fn connected_mcp_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.mcp_tools.lock().unwrap().clone()
    }

    /// Tools in registration order, narrowed by `allowed_tools` when set.
    pub fn get_tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut all: Vec<Arc<dyn Tool>> = self.tools.lock().unwrap().iter().chain(self.mcp_tools.lock().unwrap().iter()).cloned().collect();
        if let Some(allowed) = self.config.get().allowed_tools.as_ref().filter(|a| !a.is_empty()) {
            all.retain(|t| allowed.iter().any(|a| a == t.name()));
        }
        all
    }

    pub fn get_schemas(&self) -> Vec<Value> {
        self.get_tools().iter().map(|t| t.to_openai_schema()).collect()
    }

    /// The model's call with a near-miss tool name or argument fixed (tools/repair.rs), before anything acts on it.
    pub fn repair(&self, call: &crate::client::response::ToolCall) -> crate::client::response::ToolCall {
        let offered: HashSet<String> = self.get_tools().iter().map(|t| t.name().to_string()).collect();
        let name = repair::fix_tool_name(&call.name, &offered);
        let mut args = call.arguments.clone();
        if let Some(tool) = self.get(&name) {
            if !args.contains_key("raw_arguments") {
                args = repair::fix_arguments(&tool.schema(), &args);
            }
        }
        crate::client::response::ToolCall { call_id: call.call_id.clone(), name, arguments: args }
    }

    fn tracer(&self) -> Option<Arc<Tracer>> {
        self.tracer.lock().unwrap().clone()
    }

    fn target(&self, params: &Value, cwd: &Path) -> Option<PathBuf> {
        params.get("path").and_then(Value::as_str).map(|p| resolve_path(cwd, p))
    }

    pub async fn invoke(&self, name: &str, params: Value, cwd: &Path, hooks: &HookSystem, approval: Option<&ApprovalManager>, call_id: &str) -> ToolResult {
        let started = Instant::now();
        let key = format!("{name}\u{0}{}\u{0}{}", canonical_json(&params), cwd.display());
        let mut result = self.gated(name).or_else(|| self.unread_edit_error(name, &params, cwd)).or_else(|| self.repeat(name, &key));
        if result.is_none() {
            let r = self.invoke_inner(name, &params, cwd, hooks, approval, call_id).await;
            let tool = self.get(name);
            if name == "advisor" {
                if let Some(a) = tool.as_ref().and_then(|t| t.as_any().downcast_ref::<super::builtin::advisor::AdvisorTool>()) {
                    a.last_call_at.store(self.writes.load(Ordering::SeqCst) as i64, Ordering::SeqCst); // the edits so far were put to the advisor
                }
            }
            if tool.as_ref().map_or(false, |t| t.is_mutating(&params)) {
                self.writes.fetch_add(1, Ordering::SeqCst);
                crate::util::search::forget_listing();
            } else if REUSABLE.contains(&name) && r.success && !call_id.is_empty() {
                self.answered.lock().unwrap().insert(key, (call_id.to_string(), self.writes.load(Ordering::SeqCst)));
            }
            result = Some(r);
        }
        let mut result = result.unwrap();
        self.note_file_state(name, &params, cwd, &result);
        self.attach_diagnostics(name, &mut result, cwd).await;
        self.cap_output(&mut result, name);
        // after the cap, so a 2000-line read can't cut the folder's rules off its tail
        if matches!(name, "read_file" | "edit" | "write_file") && result.success {
            if let Some(path) = self.target(&params, cwd) {
                let cfg = self.config.get();
                let extra = {
                    let mut seen = self.instructions_seen.lock().unwrap();
                    crate::agent::instructions::for_path(&cfg.cwd, &path, &mut seen)
                };
                if !extra.is_empty() {
                    result.output = format!("{}\n\n{extra}", result.output);
                }
            }
        }
        if let Some(t) = self.tracer() {
            t.emit(
                "tool_call",
                json!({"tool": name, "ok": result.success, "sec": (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
                       "blocked": result.flag("blocked"), "chars": result.output.chars().count(), "target": target_label(name, &params),
                       "error": if result.success { Value::Null } else { json!(crate::util::text::head_chars(result.error.as_deref().unwrap_or(""), 200)) }}),
            );
        }
        result
    }

    /// A broken edit is reported in its own result, while the model still has the change in mind.
    async fn attach_diagnostics(&self, name: &str, result: &mut ToolResult, cwd: &Path) {
        if !matches!(name, "edit" | "write_file") || !result.success {
            return;
        }
        let Some(diff) = result.diff.clone() else { return };
        let old = if diff.is_new_file { None } else { Some(diff.old_content.as_str()) };
        let found = crate::util::diagnostics::new_problems(&diff.path, old, &diff.new_content, cwd).await;
        if !found.is_empty() {
            let rel = crate::util::paths::rel_display(&diff.path, cwd);
            result.output.push_str(&crate::util::diagnostics::render(&rel, &found));
            if let Some(t) = self.tracer() {
                t.emit("diagnostics", json!({"path": rel, "count": found.len()}));
            }
        }
    }

    /// The same lookup again, with nothing written since and the first answer still in context: say so in one line
    /// instead of sending the same text a second time.
    fn repeat(&self, name: &str, key: &str) -> Option<ToolResult> {
        if !REUSABLE.contains(&name) {
            return None;
        }
        let hit = self.answered.lock().unwrap().get(key).cloned()?;
        if hit.1 != self.writes.load(Ordering::SeqCst) || !(self.is_live.lock().unwrap().clone())(&hit.0) {
            return None;
        }
        Some(ToolResult::ok_with(
            format!("[unchanged: this exact {name} call was already answered above (call {}) and nothing was written since. Use that result, or change the query.]", hit.0),
            json!({"deduped": true, "tool_name": name}),
        ))
    }

    fn gated(&self, name: &str) -> Option<ToolResult> {
        let gate = self.gate.lock().unwrap().clone()?;
        gate(name).map(|msg| ToolResult::error_with(msg, json!({"tool_name": name, "gated": true})))
    }

    /// Editing from memory is how wrong edits happen. An existing file must have been read (and be unchanged since)
    /// before edit/write_file may touch it.
    fn unread_edit_error(&self, name: &str, params: &Value, cwd: &Path) -> Option<ToolResult> {
        if !self.config.get().require_read_before_edit || !matches!(name, "edit" | "write_file") {
            return None;
        }
        let path = self.target(params, cwd)?;
        if !path.is_file() {
            return None; // creating a new file needs no prior read
        }
        let p = params["path"].as_str().unwrap_or("");
        let seen = self.read_state.lock().unwrap().get(&path.to_string_lossy().to_string()).cloned();
        match seen {
            None => Some(ToolResult::error_with(format!("Read {p} with read_file before editing it, so the change is based on its real contents."), json!({"tool_name": name, "needs_read": true}))),
            Some(s) if s != stamp(&path) => Some(ToolResult::error_with(format!("{p} changed on disk since you read it. Re-read it before editing."), json!({"tool_name": name, "needs_read": true}))),
            _ => None,
        }
    }

    /// Files the agent read or wrote that something else has changed since: the user in their editor, a formatter, a
    /// command the agent ran. Each change is reported once; the edit tools still insist on a fresh read.
    pub fn changed_on_disk(&self) -> Vec<String> {
        let mut out = vec![];
        let state = self.read_state.lock().unwrap().clone();
        let mut announced = self.announced.lock().unwrap();
        for (path, seen) in state {
            let now = stamp(Path::new(&path));
            if now != seen && announced.get(&path).cloned().unwrap_or(seen) != now {
                announced.insert(path.clone(), now);
                out.push(if now.is_some() { path } else { format!("{path} (deleted)") });
            }
        }
        out
    }

    fn note_file_state(&self, name: &str, params: &Value, cwd: &Path, result: &ToolResult) {
        if matches!(name, "read_file" | "edit" | "write_file") && result.success {
            if let Some(path) = self.target(params, cwd) {
                if let Some(s) = stamp(&path) {
                    self.read_state.lock().unwrap().insert(path.to_string_lossy().to_string(), Some(s)); // after our own write we know the content too
                }
            }
        }
    }

    /// The result that carried a folder's AGENTS.md was cleared or compacted: the next touch there attaches it again.
    pub fn forget_instructions(&self) {
        self.instructions_seen.lock().unwrap().clear();
    }

    /// One tool result must never be able to eat the context: keep the head and the tail.
    fn cap_output(&self, result: &mut ToolResult, name: &str) {
        let cfg = self.config.get();
        if cfg.pi_mode {
            return self.pi_cap(result, name);
        }
        let limit = cfg.tool_output_max_chars;
        let chars: Vec<char> = result.output.chars().collect();
        if chars.len() > limit {
            let (head, tail) = ((limit as f64 * 0.6) as usize, (limit as f64 * 0.4) as usize);
            let rid = self.archive.lock().unwrap().put(&result.output);
            result.output = format!(
                "{}\n... [{} chars omitted; full output: {rid}, get a range with recall_result] ...\n{}",
                chars[..head].iter().collect::<String>(),
                chars.len() - limit,
                chars[chars.len() - tail..].iter().collect::<String>()
            );
            result.truncated = true;
        }
    }

    /// Pi's limit: 2000 lines or 50KB, whichever is hit first, whole lines only. Shell output keeps its tail (the error
    /// is at the end); everything else keeps its head. The full text goes to a temp file the model can open.
    fn pi_cap(&self, result: &mut ToolResult, name: &str) {
        let lines: Vec<&str> = result.output.split('\n').collect();
        let shell = name == "shell";
        let limit = if shell { SHELL_MAX_BYTES } else { PI_MAX_BYTES };
        let kept: Vec<&str> = if shell { lines.iter().rev().take(PI_MAX_LINES).cloned().collect() } else { lines.iter().take(PI_MAX_LINES).cloned().collect() };
        let (mut out, mut size) = (Vec::new(), 0usize);
        for line in kept {
            size += line.len() + 1;
            if size > limit && !out.is_empty() {
                break;
            }
            out.push(line);
        }
        if shell {
            out.reverse();
        }
        if out.len() == lines.len() {
            return;
        }
        let full = tempfile::Builder::new().prefix("melon-").suffix(".log").tempfile().ok().and_then(|f| f.keep().ok()).map(|(mut f, p)| {
            use std::io::Write;
            let _ = f.write_all(result.output.as_bytes());
            p
        });
        let which = if shell { "last" } else { "first" };
        let n = out.len();
        let body = out.join("\n");
        result.output = format!(
            "{body}\n\n[Showing {which} {n} of {} lines. Full output: {} (outside the workspace, so page it with shell: sed -n '1,200p')]",
            lines.len(),
            full.map(|p| p.display().to_string()).unwrap_or_default()
        );
        result.truncated = true;
    }

    async fn invoke_inner(&self, name: &str, params: &Value, cwd: &Path, hooks: &HookSystem, approval: Option<&ApprovalManager>, call_id: &str) -> ToolResult {
        let cfg = self.config.get();
        let Some(tool) = self.get(name) else {
            let mut names: Vec<String> = self.get_tools().iter().map(|t| t.name().to_string()).collect();
            names.sort();
            let r = ToolResult::error_with(format!("Unknown tool: {name}. The tools you can call are: {}", names.join(", ")), json!({"tool_name": name}));
            hooks.trigger_after_tool(name, params, &r).await;
            return r;
        };
        let fail = |r: ToolResult| async move {
            hooks.trigger_after_tool(name, params, &r).await;
            r
        };
        if params.as_object().map_or(false, |o| o.len() == 1 && o.contains_key("raw_arguments")) {
            // the client could not parse the model's arguments as JSON
            let raw = params["raw_arguments"].as_str().unwrap_or("");
            return fail(ToolResult::error_with(
                format!("Tool arguments were not valid JSON. Re-send the call with a well-formed JSON object. Received: {}", crate::util::text::head_chars(raw, 200)),
                json!({"tool_name": name}),
            ))
            .await;
        }
        let params = match tool.validate_params(params) {
            Ok(p) => p,
            Err(errors) => {
                return fail(ToolResult::error_with(
                    format!("Invalid parameters: {}. Received: {}", errors.join("; "), crate::util::text::head_chars(&params.to_string(), 300)),
                    json!({"tool_name": name, "validation_errors": errors}),
                ))
                .await;
            }
        };
        if let Some(blocked) = violation(name, &params, &cfg) {
            return fail(ToolResult::error_with(
                format!("Blocked by repository rules: {blocked}. Do not retry this; ask the user to provide what you need."),
                json!({"tool_name": name, "blocked": true}),
            ))
            .await;
        }
        if let Some(veto) = hooks.trigger_before_tool(name, &params).await {
            return fail(ToolResult::error_with(veto, json!({"tool_name": name, "blocked": true}))).await;
        }
        let inv = ToolInvocation { params: params.clone(), cwd: cwd.to_path_buf(), epoch: self.epoch.load(Ordering::SeqCst), call_id: call_id.to_string(), progress: self.progress.lock().unwrap().clone() };
        let rule = approval.and_then(|a| a.decide_rule(name, &params));
        if let Some(r) = &rule {
            if r.kind == "deny" {
                return fail(ToolResult::error_with(
                    format!("Denied by the permission rule {}. Do not retry it or look for a way around it; tell the user what you needed.", r.text),
                    json!({"tool_name": name, "blocked": true}),
                ))
                .await;
            }
        }
        if let Some(approval) = approval {
            if let Some(confirmation) = tool.get_confirmation(&inv).await {
                let ctx = ApprovalContext {
                    tool_name: name.to_string(),
                    is_mutating: tool.is_mutating(&params),
                    affected_paths: confirmation.affected_paths.clone(),
                    command: confirmation.command.clone(),
                    is_dangerous: confirmation.is_dangerous,
                    tool_kind: tool.kind().value().to_string(),
                };
                let mut decision = approval.check_approval(&ctx);
                match rule.as_ref().map(|r| r.kind.as_str()) {
                    Some("allow") if decision == ApprovalDecision::NeedsConfirmation => decision = ApprovalDecision::Approved, // rejections are never lifted by a rule
                    Some("ask") if decision == ApprovalDecision::Approved => decision = ApprovalDecision::NeedsConfirmation,
                    _ => {}
                }
                match decision {
                    ApprovalDecision::Rejected => {
                        let plan = approval.policy() == crate::config::ApprovalPolicy::Plan;
                        return fail(ToolResult::error(if plan {
                            "Plan mode is on: nothing may be changed. Finish investigating with read-only tools, then give the user a concrete plan (files, edits, how to verify) and ask them to leave plan mode (/plan) so you can carry it out."
                        } else {
                            "Operation rejected by safety policy"
                        }))
                        .await;
                    }
                    ApprovalDecision::NeedsConfirmation => {
                        let answer = approval.request_confirmation(confirmation.clone()).await;
                        if !answer.approved {
                            let mut msg = "User rejected the operation.".to_string();
                            if let Some(fb) = &answer.feedback {
                                msg.push(' ');
                                msg.push_str(fb);
                                learn_from_rejection(&cfg.cwd, name, &params, fb);
                            }
                            return fail(ToolResult::error_with(msg, json!({"tool_name": name, "rejected": true, "no_approver": !approval.has_callback()}))).await;
                        }
                        if answer.always {
                            approval.remember(&confirmation);
                        }
                    }
                    ApprovalDecision::Approved => {}
                }
            }
        }
        let mut result = tool.execute(&inv).await;
        if let Some(note) = hooks.trigger_after_tool(name, &params, &result).await {
            result.output = format!("{}\n\n{note}", result.output);
        }
        result
    }
}

/// JSON with object keys sorted, so equal calls make equal keys.
fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            format!("{{{}}}", keys.iter().map(|k| format!("{}:{}", Value::String((*k).clone()), canonical_json(&m[*k]))).collect::<Vec<_>>().join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(canonical_json).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

/// Short label for what a call looked at, for the token cost breakdown.
pub fn target_label(_name: &str, params: &Value) -> String {
    for key in ["symbol", "path", "pattern", "query", "command", "op", "goal", "name"] {
        if let Some(v) = params.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            let base = if matches!(key, "symbol" | "pattern") { params.get("path").and_then(Value::as_str) } else { None };
            let s = match base {
                Some(b) => format!("{b}:{v}"),
                None => v.to_string(),
            };
            return crate::util::text::head_chars(&s, 80);
        }
    }
    String::new()
}

/// What the user said when they turned an action down becomes a lesson (listed by /lessons, removable by /forget).
fn learn_from_rejection(cwd: &Path, tool: &str, params: &Value, feedback: &str) {
    let said = feedback.strip_prefix("The user said:").unwrap_or(feedback).trim();
    if !said.is_empty() {
        crate::agent::learn::Learned::new(cwd).add_lesson(&format!("After you tried {tool} {}, the user said: {said}", target_label(tool, params)), "rejection");
    }
}

/// Registers the built-in tools for this config.
pub fn create_default_registry(config: ConfigHandle) -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new(config.clone()));
    for t in super::builtin::all_builtin(&config) {
        registry.register(t);
    }
    registry.register(Arc::new(super::builtin::recall::RecallResultTool::new(registry.archive.lock().unwrap().clone())));
    let cfg = config.get();
    let in_git = crate::util::paths::resolve(&cfg.cwd).ancestors().take(6).any(|d| d.join(".git").exists());
    if cfg.enable_parallel_tasks && in_git {
        registry.register(Arc::new(super::builtin::parallel_tasks::ParallelTasksTool::new(config.clone())));
    }
    // the tool needs either a graphify snapshot or a repo big enough for the live import graph to be worth a schema
    if crate::util::codegraph::locate(&cfg.cwd).is_some() || crate::util::importgraph::tracked(&cfg.cwd).len() >= 15 {
        registry.register(Arc::new(super::builtin::graph::GraphTool::new(config.clone())));
    }
    for dir in [cfg.cwd.join(".melon").join("tools"), crate::config::loader::get_config_dir().join("tools")] {
        for t in super::discovery::discover(&dir) {
            registry.register(t);
        }
    }
    for d in subagents::default_definitions(&cfg) {
        registry.register(Arc::new(subagents::SubagentTool::new(config.clone(), d)));
    }
    registry
}
