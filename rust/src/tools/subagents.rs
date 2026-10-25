//! Sub-agents: a goal handed to a fresh agent with its own context, tools and time budget. A read-only one cannot
//! conflict with anything, so several run at once and need no approval.
use super::*;
use crate::agent::agent::Agent;
use crate::agent::events::AgentEvent;
use crate::config::{Config, ConfigHandle, HookTrigger, SubagentSpec};
use crate::util::re::{lit, lit_i};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Semaphore;

/// A sub-agent's answer lands in the parent's context for the rest of the run.
const MAX_REPORT_CHARS: usize = 6000;
pub const READONLY_TOOLS: &[&str] = &["read_file", "grep", "search", "outline", "git", "glob", "list_dir", "use_skill", "graph", "web_search", "web_fetch"];

#[derive(Clone, Debug)]
pub struct SubagentDefinition {
    pub name: String,
    pub description: String,
    pub goal_prompt: String,
    pub allowed_tools: Option<Vec<String>>,
    pub max_turns: usize,
    pub timeout_seconds: f64,
    pub role: String,
}

impl From<&SubagentSpec> for SubagentDefinition {
    fn from(s: &SubagentSpec) -> Self {
        SubagentDefinition { name: s.name.clone(), description: s.description.clone(), goal_prompt: s.goal_prompt.clone(), allowed_tools: s.allowed_tools.clone(), max_turns: s.max_turns, timeout_seconds: s.timeout_seconds, role: s.role.clone() }
    }
}

fn slots(limit: usize) -> Arc<Semaphore> {
    /// One cap shared by every sub-agent tool in this process.
    static S: std::sync::OnceLock<Mutex<HashMap<usize, Arc<Semaphore>>>> = std::sync::OnceLock::new();
    let n = limit.max(1);
    S.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap().entry(n).or_insert_with(|| Arc::new(Semaphore::new(n))).clone()
}

pub struct SubagentTool {
    config: ConfigHandle,
    pub definition: SubagentDefinition,
    name: String,
    readonly: bool,
}

impl SubagentTool {
    pub fn new(config: ConfigHandle, definition: SubagentDefinition) -> Self {
        let readonly = definition.allowed_tools.as_ref().map_or(false, |t| !t.is_empty() && t.iter().all(|x| READONLY_TOOLS.contains(&x.as_str())));
        SubagentTool { name: format!("subagent_{}", definition.name), config, definition, readonly }
    }
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> String {
        self.definition.description.clone()
    }
    fn kind(&self) -> ToolKind {
        if self.readonly { ToolKind::Read } else { ToolKind::Shell }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn is_mutating(&self, _p: &Value) -> bool {
        !self.readonly
    }
    fn schema(&self) -> Value {
        schema_of(json!({"goal": {"type": "string", "description": "The specific task or goal for the subagent to accomplish"}}), &["goal"])
    }

    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let goal = inv.str("goal").unwrap_or("").to_string();
        if goal.is_empty() {
            return ToolResult::error("No goal specified for sub-agent");
        }
        let parent = self.config.get();
        let mut cfg = (*parent).clone();
        cfg.max_turns = self.definition.max_turns;
        cfg.model_role = self.definition.role.clone();
        cfg.auto_review = false;
        cfg.subagents = vec![]; // no recursive delegation
        cfg.enable_parallel_tasks = false;
        cfg.mcp_servers = Default::default(); // starting every MCP server per sub-agent costs seconds and the task is local
        cfg.auto_verify = false; // the parent run verifies the final tree
        cfg.verify_commands = vec![];
        if let Some(t) = &self.definition.allowed_tools {
            cfg.allowed_tools = Some(t.clone());
        }
        let prompt = format!(
            "You are a specialized sub-agent with a specific task to complete.\n\n{}\n\nYOUR TASK:\n{goal}\n\nIMPORTANT:\n- Focus only on completing the specified task\n- Do not engage in unrelated actions\n- Once you have completed the task or have the answer, provide your final response\n- Be concise and direct in your output\n",
            self.definition.goal_prompt
        );
        let (mut tool_calls, mut final_response, mut error, mut terminate, mut tokens): (Vec<String>, Option<String>, Option<String>, &str, i64) = (vec![], None, None, "goal", 0);
        let name = self.definition.name.clone();
        let report = |info: Value| {
            if let Some(p) = &inv.progress {
                let mut v = json!({"agent": name});
                if let (Some(a), Some(b)) = (v.as_object_mut(), info.as_object()) {
                    a.extend(b.clone());
                }
                p(&inv.call_id, v);
            }
        };

        let permit = slots(parent.max_parallel_agents).acquire_owned().await.ok();
        report(json!({"kind": "start", "goal": goal}));
        let started = async {
            let mut agent = Agent::new(cfg, None).await;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
            {
            let run = agent.run(&prompt, tx);
            tokio::pin!(run);
            let handle = |ev: AgentEvent, tool_calls: &mut Vec<String>, final_response: &mut Option<String>, error: &mut Option<String>, terminate: &mut &str, tokens: &mut i64| -> bool {
                match ev {
                    AgentEvent::ToolCallStart { name, arguments, tokens: t, .. } => {
                        *tokens = t;
                        tool_calls.push(name.clone());
                        report(json!({"kind": "tool", "name": name, "args": arguments, "tokens": t}));
                    }
                    AgentEvent::TextComplete(c) => *final_response = Some(c),
                    AgentEvent::AgentEnd { response, .. } => {
                        if final_response.is_none() {
                            *final_response = response;
                        }
                    }
                    AgentEvent::AgentError { error: e, .. } => {
                        *terminate = "error";
                        *final_response = Some(format!("Sub-agent error: {e}"));
                        *error = Some(e);
                        return true;
                    }
                    _ => {}
                }
                false
            };
            let mut stop = false;
            loop {
                tokio::select! {
                    _ = &mut run => break,
                    Some(ev) = rx.recv() => { if handle(ev, &mut tool_calls, &mut final_response, &mut error, &mut terminate, &mut tokens) { stop = true; break; } }
                }
            }
            if !stop {
                while let Ok(ev) = rx.try_recv() {
                    if handle(ev, &mut tool_calls, &mut final_response, &mut error, &mut terminate, &mut tokens) {
                        break;
                    }
                }
            }
            }
            agent.close().await;
        };
        // a timeout cuts a hung model call or tool too; checking the clock between events could not
        if tokio::time::timeout(Duration::from_secs_f64(self.definition.timeout_seconds), started).await.is_err() {
            terminate = "timeout";
            final_response = final_response.or_else(|| Some(format!("Sub-agent timed out after {:.0}s", self.definition.timeout_seconds)));
        }
        drop(permit);

        report(json!({"kind": "done", "ok": error.is_none(), "tokens": tokens}));
        crate::hooks::HookSystem::new(&parent).fire(HookTrigger::SubagentStop, &[("agent", self.definition.name.clone()), ("ok", (error.is_none()).to_string())]).await;
        if let (Some(fr), None) = (&final_response, &error) {
            // off the loop: listing a big repo for same-name files takes ~2.5s on tunyt
            let (cwd, text, forbidden) = (inv.cwd.clone(), fr.clone(), parent.forbidden_paths.clone());
            let notes = tokio::task::spawn_blocking(move || crate::util::search::check_paths(&cwd, &text, &forbidden)).await.unwrap_or_default();
            if !notes.is_empty() {
                final_response = Some(format!("{notes}\n\n{fr}")); // first, so a caller that cuts the report keeps them
            }
        }
        if let Some(fr) = &final_response {
            if fr.chars().count() > MAX_REPORT_CHARS {
                final_response = Some(format!("{}\n... [report cut; ask for a narrower answer]", fr.chars().take(MAX_REPORT_CHARS).collect::<String>()));
            }
        }
        let result = format!(
            "Sub-agent '{}' completed. \nTermination: {terminate}\nTools called: {}\n\nResult:\n{}\n",
            self.definition.name,
            if tool_calls.is_empty() { "None".to_string() } else { tool_calls.join(", ") },
            final_response.as_deref().filter(|s| !s.is_empty()).unwrap_or("No response")
        );
        match error {
            // the reason goes first: UIs show only the top lines of a result
            Some(e) => ToolResult::error(format!("Sub-agent '{}' FAILED: {e}\n{result}", self.definition.name)),
            None => ToolResult::ok(result),
        }
    }
}

fn def(name: &str, description: &str, goal_prompt: &str, tools: Option<&[&str]>, max_turns: usize, timeout: f64, role: &str) -> SubagentDefinition {
    SubagentDefinition {
        name: name.into(),
        description: description.into(),
        goal_prompt: goal_prompt.into(),
        allowed_tools: tools.map(|t| t.iter().map(|s| s.to_string()).collect()),
        max_turns,
        timeout_seconds: timeout,
        role: role.into(),
    }
}

const SEARCH_TOOLS: &[&str] = &["read_file", "grep", "glob", "list_dir", "search", "outline", "graph", "git"];

fn codebase_investigator() -> SubagentDefinition {
    def(
        "codebase_investigator",
        "Read-only search agent for questions that need many files: where something lives, who calls it, how a flow works. Returns the answer with file:line evidence, not file dumps. Launch several in one turn for independent questions",
        "You are a codebase search specialist. Answer the question with evidence and keep it short.\nStart broad (graph, grep, glob, outline), then read only the lines that settle it.\nReport file:line for every claim, and say what you could not find. Do NOT modify any files.",
        Some(SEARCH_TOOLS),
        15,
        600.0,
        "explore",
    )
}

fn planner() -> SubagentDefinition {
    def(
        "planner",
        "Read-only planning agent: given a feature or change, studies the code and returns an implementation plan (files, steps, risks, how to verify). It never edits",
        "You are a software architect. Study how the repo already does the nearest thing, then return a plan:\n1) the goal in one line, 2) the files to change and what changes in each, in order, 3) risks and what could break (callers, migrations, permissions),\n4) how to verify. Prefer the smallest plan that works. Do NOT modify any files.",
        Some(SEARCH_TOOLS),
        15,
        600.0,
        "default",
    )
}

fn code_reviewer() -> SubagentDefinition {
    def(
        "code_reviewer",
        "Reviews code changes and provides feedback on quality, bugs, and improvements",
        "You are a code review specialist.\nYour job is to review code and provide constructive feedback.\nLook for bugs, code smells, security issues, and improvement opportunities.\nUse read_file, list_dir and grep to examine the code.\nDo NOT modify any files.",
        Some(&["read_file", "grep", "list_dir"]),
        10,
        300.0,
        "default",
    )
}

fn general() -> SubagentDefinition {
    def(
        "general",
        "General-purpose agent for a self-contained job in its own context: an independent second opinion on a commit or diff, multi-step research, a bounded fix in one area. Give it the full goal and what done looks like; it cannot ask you questions",
        "You are a capable engineer handed one self-contained job. Do it end to end with the tools you have,\ncheck your work, and report what you found or changed with file:line evidence. If the job is a review, report only\nproblems you verified. If you cannot finish, say what blocked you.",
        None,
        25,
        600.0,
        "default",
    )
}

/// Claude Code's tool names, so an agent file written for it works here unchanged.
fn claude_tool(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "read" => "read_file",
        "grep" => "grep",
        "glob" => "glob",
        "ls" => "list_dir",
        "bash" => "shell",
        "edit" | "multiedit" => "edit",
        "write" => "write_file",
        "webfetch" => "web_fetch",
        "websearch" => "web_search",
        _ => return name.to_string(),
    }
    .to_string()
}

fn agent_dirs(cwd: &Path) -> Vec<std::path::PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    vec![home.join(".claude/agents"), home.join(".melon/agents"), cwd.join(".claude/agents"), cwd.join(".melon/agents")]
}

/// A markdown file like Claude Code's `.claude/agents/reviewer.md` (front matter: name, description, tools, model).
pub fn parse_agent_file(text: &str) -> Option<SubagentDefinition> {
    let rest = text.strip_prefix("---\n")?;
    let (head, body) = rest.split_once("\n---").map(|(h, b)| (h, b.trim_start_matches('\n')))?;
    let mut meta: HashMap<String, String> = HashMap::new();
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if !line.starts_with(char::is_whitespace) {
                meta.insert(k.trim().to_lowercase(), v.split("  #").next().unwrap_or("").trim().trim_matches(|c| c == '\'' || c == '"').to_string());
            }
        }
    }
    let name = lit_i(r"[^a-z0-9_]+").replace_all(&meta.get("name")?.to_lowercase(), "_").trim_matches('_').to_string();
    if name.is_empty() || body.trim().is_empty() {
        return None;
    }
    let mut tools: Vec<String> = vec![];
    for t in meta.get("tools").map(String::as_str).unwrap_or("").split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let t = claude_tool(t);
        if !tools.contains(&t) {
            tools.push(t);
        }
    }
    let turns = meta.get("maxturns").or(meta.get("max_turns")).and_then(|t| t.parse::<usize>().ok()).unwrap_or(20);
    Some(SubagentDefinition {
        description: meta.get("description").filter(|d| !d.is_empty()).cloned().unwrap_or_else(|| format!("Custom agent {name}")),
        goal_prompt: body.trim().to_string(),
        allowed_tools: if tools.is_empty() { None } else { Some(tools) },
        max_turns: turns,
        timeout_seconds: 600.0,
        role: if meta.get("model").map_or(false, |m| m.to_lowercase() == "haiku") { "fast".into() } else { "default".into() },
        name,
    })
}

pub fn load_agent_files(cwd: &Path) -> Vec<SubagentDefinition> {
    let mut found: Vec<SubagentDefinition> = vec![];
    for d in agent_dirs(cwd) {
        // later folders (the repo's own) override earlier ones
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "md")).collect();
        files.sort();
        for f in files {
            if let Some(def) = std::fs::read_to_string(&f).ok().and_then(|t| parse_agent_file(&t)) {
                found.retain(|x| x.name != def.name);
                found.push(def);
            }
        }
    }
    found
}

/// The agents a repo's profile or config declares first, then the built-ins it did not already cover, then the user's
/// agent files. A profile that brings its own investigator replaces the built-in one rather than doubling it.
pub fn default_definitions(config: &Config) -> Vec<SubagentDefinition> {
    let mut defs: Vec<SubagentDefinition> = config.subagents.iter().map(SubagentDefinition::from).collect();
    let covers_search = defs.iter().any(|d| lit(r"investigat|explor").is_match(&d.name));
    for (i, d) in [codebase_investigator(), planner(), code_reviewer(), general()].into_iter().enumerate() {
        if !defs.iter().any(|x| x.name == d.name) && !(i == 0 && covers_search) {
            defs.push(d);
        }
    }
    for d in load_agent_files(&config.cwd) {
        defs.retain(|x| x.name != d.name);
        defs.push(d);
    }
    defs
}
