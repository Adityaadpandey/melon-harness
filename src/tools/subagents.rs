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
        schema_of(
            json!({
                "goal": {"type": "string", "description": "The specific task or goal for the subagent to accomplish. It has seen none of your conversation: include what it needs"},
                "background": {"type": "boolean", "default": false, "description": "Start it and return at once, so you can keep working. Its report is delivered to you when it finishes; do not wait, poll or start the same job twice"},
                "resume": {"type": ["string", "null"], "description": "Agent id from an earlier result: continue that agent with `goal` as a follow-up. It still has everything it read, so this is cheaper than a fresh agent"},
            }),
            &["goal"],
        )
    }

    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let goal = inv.str("goal").unwrap_or("").to_string();
        if goal.is_empty() {
            return ToolResult::error("No goal specified for sub-agent");
        }
        let id = inv.str("resume").filter(|r| !r.is_empty()).map(String::from).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()[..8].to_string());
        let job = Job { def: self.definition.clone(), parent: self.config.get(), goal, resume: inv.str("resume").map_or(false, |r| !r.is_empty()), owner: self.config.key(), id: id.clone(), progress: inv.progress.clone(), call_id: inv.call_id.clone(), cwd: inv.cwd.clone() };
        if inv.params["background"].as_bool().unwrap_or(false) {
            let (owner, name) = (self.config.key(), self.definition.name.clone());
            let (goal, bid) = (job.goal.clone(), id.clone());
            // the board is held across the spawn so the task cannot finish before its row exists
            let mut b = board().lock().unwrap();
            let task = tokio::spawn(async move {
                let result = run_job(job).await;
                let mut b = board().lock().unwrap();
                b.running.retain(|r| r.id != id);
                b.done.push((owner, id, name, result.to_model_output()));
            });
            b.running.push(Running { owner, id: bid, name: self.definition.name.clone(), goal, started: std::time::Instant::now(), task: task.abort_handle() });
            drop(b);
            return ToolResult::ok(format!("Started sub-agent '{}' in the background. Its report is delivered to you when it finishes: carry on with other work, do not wait or poll, and do not start the same job again.", self.definition.name));
        }
        run_job(job).await
    }
}

/// The time a sub-agent may work before it must stop and answer from what it has. It sits below the hard timeout so
/// that last answer still fits: a 600s agent wraps up at 510s instead of being cut at 600s with nothing to show.
pub fn soft_budget(hard_timeout: f64) -> f64 {
    (hard_timeout - 90.0).max(hard_timeout * 0.5)
}

/// Everything one sub-agent run needs, owned, so it can also run as a background task.
struct Job {
    def: SubagentDefinition,
    parent: Arc<Config>,
    goal: String,
    resume: bool,
    /// The parent's config key: whose total this run's tokens are added to.
    owner: usize,
    id: String,
    progress: Option<ProgressFn>,
    call_id: String,
    cwd: std::path::PathBuf,
}

/// Background sub-agents: which are still running and which reports wait to be handed to their parent. Each entry
/// carries the parent's config key, so two agents in one process never read each other's reports.
#[derive(Default)]
pub struct Board {
    running: Vec<Running>,
    done: Vec<(usize, String, String, String)>, // owner, agent id, agent name, report
}

struct Running {
    owner: usize,
    id: String,
    name: String,
    goal: String,
    started: std::time::Instant,
    task: tokio::task::AbortHandle,
}

/// This parent's background sub-agents still at work: (agent id, name, goal, seconds so far). What `/agents` shows.
pub fn running(owner: usize) -> Vec<(String, String, String, u64)> {
    board().lock().unwrap().running.iter().filter(|r| r.owner == owner).map(|r| (r.id.clone(), r.name.clone(), r.goal.clone(), r.started.elapsed().as_secs())).collect()
}

/// Stop one background sub-agent (`/agents stop ab12cd34`). Its parent is told it was stopped, so it does not wait
/// for a report that will never come.
pub fn stop(owner: usize, id: &str) -> bool {
    let mut b = board().lock().unwrap();
    let Some(i) = b.running.iter().position(|r| r.owner == owner && r.id == id) else { return false };
    let r = b.running.remove(i);
    r.task.abort();
    b.done.push((owner, r.id, r.name, "Stopped by the user before it finished. It has no report; do not start it again unless asked.".into()));
    true
}

/// Tokens spent by sub-agents and parallel workers, waiting to be added to their parent's total. Each one keeps its
/// own counter, so without this a request that delegated most of its reading looked almost free in /cost.
fn child_usage() -> &'static Mutex<HashMap<usize, crate::client::response::TokenUsage>> {
    static U: std::sync::OnceLock<Mutex<HashMap<usize, crate::client::response::TokenUsage>>> = std::sync::OnceLock::new();
    U.get_or_init(Default::default)
}

pub fn add_usage(owner: usize, u: &crate::client::response::TokenUsage) {
    child_usage().lock().unwrap().entry(owner).or_default().add(u);
}

/// What this parent's sub-agents spent since the last call.
pub fn take_usage(owner: usize) -> Option<crate::client::response::TokenUsage> {
    child_usage().lock().unwrap().remove(&owner)
}

fn board() -> &'static Mutex<Board> {
    static B: std::sync::OnceLock<Mutex<Board>> = std::sync::OnceLock::new();
    B.get_or_init(Default::default)
}

/// Reports of this parent's background sub-agents that finished since the last call: (agent id, name, report).
pub fn finished(owner: usize) -> Vec<(String, String, String)> {
    let mut b = board().lock().unwrap();
    let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut b.done).into_iter().partition(|d| d.0 == owner);
    b.done = rest;
    mine.into_iter().map(|(_, id, name, report)| (id, name, report)).collect()
}

/// How many of this parent's background sub-agents are still working or have a report waiting.
pub fn pending(owner: usize) -> usize {
    let b = board().lock().unwrap();
    b.running.iter().filter(|r| r.owner == owner).count() + b.done.iter().filter(|d| d.0 == owner).count()
}

/// Returns once one of this parent's background sub-agents has a report ready, or none is left running.
pub async fn wait_for_report(owner: usize) {
    loop {
        {
            let b = board().lock().unwrap();
            if b.done.iter().any(|d| d.0 == owner) || !b.running.iter().any(|r| r.owner == owner) {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
pub fn track(owner: usize, id: &str, name: &str, task: tokio::task::AbortHandle) {
    board().lock().unwrap().running.push(Running { owner, id: id.into(), name: name.into(), goal: "g".into(), started: std::time::Instant::now(), task });
}

#[cfg(test)]
pub fn deliver(owner: usize, id: &str, name: &str, report: &str) {
    board().lock().unwrap().done.push((owner, id.into(), name.into(), report.into()));
}

/// Finished sub-agents kept so they can be continued with `resume`. Only the last few: each holds a whole conversation.
const KEPT: usize = 6;

fn kept() -> &'static Mutex<Vec<(String, Agent)>> {
    static K: std::sync::OnceLock<Mutex<Vec<(String, Agent)>>> = std::sync::OnceLock::new();
    K.get_or_init(Default::default)
}

async fn run_job(job: Job) -> ToolResult {
    let Job { def, parent, goal, resume, owner, id, progress, call_id, cwd } = job;
    let mut cfg = (*parent).clone();
    cfg.max_turns = def.max_turns;
    cfg.max_seconds = soft_budget(def.timeout_seconds);
    cfg.model_role = def.role.clone();
    cfg.auto_review = false;
    cfg.subagents = vec![]; // no recursive delegation
    cfg.enable_parallel_tasks = false;
    cfg.mcp_servers = Default::default(); // starting every MCP server per sub-agent costs seconds and the task is local
    cfg.auto_verify = false; // the parent run verifies the final tree
    cfg.verify_commands = vec![];
    if let Some(t) = &def.allowed_tools {
        cfg.allowed_tools = Some(t.clone());
    }
    let earlier = if resume {
        let mut k = kept().lock().unwrap();
        let Some(i) = k.iter().position(|(kid, _)| *kid == id) else {
            return ToolResult::error(format!("No sub-agent '{id}' to resume: only the last {KEPT} finished ones are kept. Start a fresh one and give it what it needs."));
        };
        Some(k.remove(i).1)
    } else {
        None
    };
    // a resumed agent already has its role and everything it read: the follow-up goes in as it is
    let prompt = if resume {
        goal.clone()
    } else {
        format!(
            "You are a specialized sub-agent with a specific task to complete.\n\n{}\n\nYOUR TASK:\n{goal}\n\nIMPORTANT:\n- Focus only on completing the specified task\n- Do not engage in unrelated actions\n- Once you have completed the task or have the answer, provide your final response\n- Be concise and direct in your output\n",
            def.goal_prompt
        )
    };
    let (mut tool_calls, mut final_response, mut error, mut terminate, mut tokens): (Vec<String>, Option<String>, Option<String>, &str, i64) = (vec![], None, None, "goal", 0);
    let name = def.name.clone();
    let report = |info: Value| {
        if let Some(p) = &progress {
            let mut v = json!({"agent": name});
            if let (Some(a), Some(b)) = (v.as_object_mut(), info.as_object()) {
                a.extend(b.clone());
            }
            p(&call_id, v);
        }
    };

    let permit = slots(parent.max_parallel_agents).acquire_owned().await.ok();
    report(json!({"kind": "start", "goal": goal}));
    let started = async {
        let mut agent = match earlier {
            Some(a) => a,
            None => Agent::new(cfg, None).await,
        };
        // a resumed agent's counter already holds its earlier runs, which the parent has been charged for
        let before = agent.session.context_manager.lock().unwrap().total_usage.clone();
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
        // ponytail: a run cut by the hard timeout never gets here, so its tokens go uncounted; the soft budget makes that rare
        add_usage(owner, &agent.session.context_manager.lock().unwrap().total_usage.since(&before));
        agent
    };
    // a timeout cuts a hung model call or tool too; checking the clock between events could not
    let mut resumable = false;
    match tokio::time::timeout(Duration::from_secs_f64(def.timeout_seconds), started).await {
        Err(_) => {
            terminate = "timeout";
            final_response = final_response.or_else(|| Some(format!("Sub-agent timed out after {:.0}s", def.timeout_seconds)));
        }
        Ok(mut agent) if error.is_some() => agent.close().await,
        Ok(agent) => {
            // kept for a follow-up; the oldest one makes room and is shut down
            resumable = true;
            let dropped = {
                let mut k = kept().lock().unwrap();
                k.push((id.clone(), agent));
                (k.len() > KEPT).then(|| k.remove(0).1)
            };
            if let Some(mut old) = dropped {
                old.close().await;
            }
        }
    }
    drop(permit);

    report(json!({"kind": "done", "ok": error.is_none(), "tokens": tokens}));
    crate::hooks::HookSystem::new(&parent).fire(HookTrigger::SubagentStop, &[("agent", def.name.clone()), ("ok", (error.is_none()).to_string())]).await;
    if let (Some(fr), None) = (&final_response, &error) {
        // off the loop: listing a big repo for same-name files takes ~2.5s on tunyt
        let (cwd, text, forbidden) = (cwd.clone(), fr.clone(), parent.forbidden_paths.clone());
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
        "Sub-agent '{}' completed. \nTermination: {terminate}\nTools called: {}\n\nResult:\n{}\n{}",
        def.name,
        if tool_calls.is_empty() { "None".to_string() } else { tool_calls.join(", ") },
        final_response.as_deref().filter(|s| !s.is_empty()).unwrap_or("No response"),
        if resumable { format!("\nAgent id: {id} (to ask this agent a follow-up, call the tool again with resume=\"{id}\": it still has everything it read)\n") } else { String::new() }
    );
    match error {
        // the reason goes first: UIs show only the top lines of a result
        Some(e) => ToolResult::error(format!("Sub-agent '{}' FAILED: {e}\n{result}", def.name)),
        None => ToolResult::ok(result),
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
        "You are a codebase search specialist. READ-ONLY: you cannot create, edit, move or delete anything, and you have no tool that could; do not try.\nBe fast: start broad (graph for a known name, grep and glob for words, outline for a file's shape), send independent lookups as parallel calls in one turn, then read only the lines that settle the question. Try a second name or spelling before saying something is not there.\nYour answer is all the caller sees, and it has not seen what you read. Give: the direct answer first; then each claim with its file:line; then the 3-5 files that matter most, one line each on why; then what you looked for and did not find. No file dumps, no preamble.",
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
        "You are a software architect. READ-ONLY: you cannot create, edit, move or delete anything; your output is the plan.\nFirst find how this repo already does the nearest thing (graph callers/importers, grep) and follow that pattern rather than inventing one. Then return:\n1) the goal in one line; 2) the files to change, in order, and what changes in each (name the function, not just the file); 3) what could break: callers, migrations, permissions, config; 4) how to verify, as commands; 5) the 3-5 files someone must read before starting, with file:line.\nPrefer the smallest plan that works, and say which parts are assumptions you could not confirm.",
        Some(SEARCH_TOOLS),
        15,
        600.0,
        "default",
    )
}

fn code_reviewer() -> SubagentDefinition {
    def(
        "code_reviewer",
        "Read-only reviewer for a diff, a commit or named files: reports verified bugs with file:line and a fix. Tell it what changed (\"the uncommitted diff\", a commit, or the files)",
        "You are a code reviewer. READ-ONLY: you cannot edit anything; your output is the review.\nStart from what changed: the git tool's `scope` then `patch` (or `show` for a commit), then read the changed functions in full. For every function whose signature or behaviour changed, ask the graph who uses it (`graph callers NAME`) and open the callers that could break; that is where most real bugs hide.\nReport only defects you confirmed by reading the code, worst first, as `severity file:line  what is wrong -> fix`. Leave out style, naming and anything you could not confirm. If you found nothing, say so in one line and name what you checked.",
        Some(&["read_file", "grep", "glob", "list_dir", "outline", "graph", "git"]),
        15,
        300.0,
        "default",
    )
}

fn general() -> SubagentDefinition {
    def(
        "general",
        "General-purpose agent for a self-contained job in its own context: an independent second opinion on a commit or diff, multi-step research, a bounded fix in one area. Give it the full goal and what done looks like; it cannot ask you questions",
        "You are a capable engineer handed one self-contained job. Do it end to end with the tools you have,\ncheck your work, and report what you found or changed with file:line evidence. If the job is a review, report only\nproblems you verified. If you cannot finish, say what blocked you.\nYour final message is all the caller sees: lead with the result, list every file you changed, and keep what you ran and saw apart from what you assume.",
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
    let home = crate::util::platform::home_dir().unwrap_or_default();
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
