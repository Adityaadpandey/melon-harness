//! Configuration: a typed `Config` filled from `config.toml` (user, then project), the profile and the CLI.
pub mod loader;
pub mod trust;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub const DEFAULT_BASE_URL: &str = "https://api.commandcode.ai/provider/v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub name: String,
    /// None: the model family's tested setting (client::llm_client SAMPLING), else the server's own default.
    pub temperature: Option<f64>,
    pub context_window: usize,
    /// Sent as max_tokens to OpenAI-style servers; shrunk to the room left in the context.
    pub max_output_tokens: usize,
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig { name: "MiniMaxAI/MiniMax-M3".into(), temperature: None, context_window: 256_000, max_output_tokens: 32_000 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellEnvironmentPolicy {
    pub ignore_default_excludes: bool,
    pub exclude_patterns: Vec<String>,
    pub set_vars: BTreeMap<String, String>,
}

impl Default for ShellEnvironmentPolicy {
    fn default() -> Self {
        ShellEnvironmentPolicy {
            ignore_default_excludes: false,
            exclude_patterns: vec!["*KEY*".into(), "*TOKEN*".into(), "*SECRET*".into()],
            set_vars: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MCPServerConfig {
    pub enabled: bool,
    pub startup_timeout_sec: f64,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub url: Option<String>,
}

impl Default for MCPServerConfig {
    fn default() -> Self {
        MCPServerConfig { enabled: true, startup_timeout_sec: 10.0, command: None, args: vec![], env: BTreeMap::new(), cwd: None, url: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalPolicy {
    #[serde(rename = "on-request")]
    OnRequest,
    #[serde(rename = "on-failure")]
    OnFailure,
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "auto-edit")]
    AutoEdit,
    #[serde(rename = "never")]
    Never,
    #[serde(rename = "yolo")]
    Yolo,
    /// Read-only, like Claude Code's plan mode: look around, write nothing, present a plan.
    #[serde(rename = "plan")]
    Plan,
}

impl ApprovalPolicy {
    pub fn value(&self) -> &'static str {
        match self {
            ApprovalPolicy::OnRequest => "on-request",
            ApprovalPolicy::OnFailure => "on-failure",
            ApprovalPolicy::Auto => "auto",
            ApprovalPolicy::AutoEdit => "auto-edit",
            ApprovalPolicy::Never => "never",
            ApprovalPolicy::Yolo => "yolo",
            ApprovalPolicy::Plan => "plan",
        }
    }
    pub fn parse(s: &str) -> Option<ApprovalPolicy> {
        Some(match s {
            "on-request" => ApprovalPolicy::OnRequest,
            "on-failure" => ApprovalPolicy::OnFailure,
            "auto" => ApprovalPolicy::Auto,
            "auto-edit" => ApprovalPolicy::AutoEdit,
            "never" => ApprovalPolicy::Never,
            "yolo" => ApprovalPolicy::Yolo,
            "plan" => ApprovalPolicy::Plan,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookTrigger {
    BeforeAgent,
    AfterAgent,
    BeforeTool,
    AfterTool,
    OnError,
    PreCompact,
    PostCompact,
    SessionStart,
    Stop,
    SubagentStop,
}

impl HookTrigger {
    pub fn value(&self) -> &'static str {
        match self {
            HookTrigger::BeforeAgent => "before_agent",
            HookTrigger::AfterAgent => "after_agent",
            HookTrigger::BeforeTool => "before_tool",
            HookTrigger::AfterTool => "after_tool",
            HookTrigger::OnError => "on_error",
            HookTrigger::PreCompact => "pre_compact",
            HookTrigger::PostCompact => "post_compact",
            HookTrigger::SessionStart => "session_start",
            HookTrigger::Stop => "stop",
            HookTrigger::SubagentStop => "subagent_stop",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookConfig {
    pub name: String,
    pub trigger: HookTrigger,
    pub command: Option<String>,
    pub script: Option<String>,
    /// Regex on the tool name for before_tool/after_tool, e.g. "edit|write_file"; empty = every tool.
    pub matcher: Option<String>,
    #[serde(default = "d30")]
    pub timeout_sec: f64,
    #[serde(default = "dtrue")]
    pub enabled: bool,
}

fn d30() -> f64 {
    30.0
}
fn dtrue() -> bool {
    true
}

/// USD per 1M tokens. Left empty by default: prices are not invented here.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cached_input: Option<f64>,
}

/// A subagent a profile can define without touching code.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentSpec {
    pub name: String,
    pub description: String,
    pub goal_prompt: String,
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default = "d20")]
    pub max_turns: usize,
    #[serde(default = "d600")]
    pub timeout_seconds: f64,
    #[serde(default = "ddefault")]
    pub role: String,
}

fn d20() -> usize {
    20
}
fn d600() -> f64 {
    600.0
}
fn ddefault() -> String {
    "default".into()
}

/// Run `commands` when a changed file starts with one of `paths`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifyRule {
    pub paths: Vec<String>,
    pub commands: Vec<String>,
    #[serde(default = "ddot")]
    pub cwd: String,
    pub tests_glob: Option<String>,
}

fn ddot() -> String {
    ".".into()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    /// Present in the table at all (even as null): the key is then honoured over the built-in's.
    #[serde(skip)]
    pub api_key_env_set: bool,
    pub protocol: Option<String>,
    pub headers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub model: ModelConfig,
    pub cwd: PathBuf,
    pub shell_environment: ShellEnvironmentPolicy,
    pub hooks_enabled: bool,
    pub hooks: Vec<HookConfig>,
    pub approval: ApprovalPolicy,
    pub max_turns: usize,
    pub mcp_servers: BTreeMap<String, MCPServerConfig>,
    pub routing: BTreeMap<String, Vec<String>>,
    pub providers: BTreeMap<String, ProviderConfig>,
    pub trace: bool,
    pub zdr: bool,
    pub verify_commands: Vec<String>,
    pub verify_rules: Vec<VerifyRule>,
    pub forbidden_paths: Vec<String>,
    pub forbidden_commands: Vec<String>,
    pub profile: Option<String>,
    pub allowed_dirs: Vec<String>,
    pub sandbox: String,
    pub sandbox_write_extra: Vec<String>,
    pub allow_private_network: bool,
    pub subagents: Vec<SubagentSpec>,
    pub disabled_skills: Vec<String>,
    pub skill_dirs: Vec<String>,
    pub model_role: String,
    pub auto_orient: bool,
    pub sub_agent: bool,
    pub auto_route: bool,
    pub reasoning_effort: Option<String>,
    pub model_pinned: bool,
    pub max_parallel_agents: usize,
    pub enable_parallel_tasks: bool,
    pub auto_challenge: bool,
    pub auto_learn: bool,
    pub auto_scout: bool,
    pub advisor: bool,
    pub advisor_model: String,
    pub advisor_max_calls: usize,
    pub auto_review: bool,
    pub max_reviews: usize,
    pub review_pipeline: bool,
    pub review_pipeline_role: String,
    pub review_pipeline_tokens: usize,
    pub review_lenses: usize,
    pub review_backend: String,
    pub review_claude_model: String,
    pub review_run_checks: bool,
    pub review_min_confidence: i64,
    pub review_repro: bool,
    pub review_gap_pass: bool,
    pub second_reviewer: bool,
    pub check_references: bool,
    pub require_read_before_edit: bool,
    pub auto_verify: bool,
    pub max_repairs: usize,
    pub verify_timeout_sec: u64,
    pub max_total_tokens: i64,
    pub tool_output_max_chars: usize,
    pub pricing: HashMap<String, ModelPrice>,
    pub max_seconds: f64,
    pub compact_at_tokens: usize,
    pub read_call_budget: usize,
    pub write_call_budget: usize,
    pub prune_at_tokens: usize,
    pub cache_ttl_seconds: u64,
    pub idle_compact_at_tokens: usize,
    pub keep_recent_tokens: usize,
    pub allowed_tools: Option<Vec<String>>,
    pub developer_instructions: Option<String>,
    pub user_instructions: Option<String>,
    pub debug: bool,
    pub ignored_project_keys: Vec<String>,
    pub pi_mode: bool,
    pub core_tools_only: bool,
    pub repo_map: bool,
}

impl Default for Config {
    fn default() -> Self {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        Config {
            model: ModelConfig::default(),
            cwd: std::env::current_dir().unwrap_or_default(),
            shell_environment: ShellEnvironmentPolicy::default(),
            hooks_enabled: false,
            hooks: vec![],
            approval: ApprovalPolicy::OnRequest,
            max_turns: 100,
            mcp_servers: BTreeMap::new(),
            routing: BTreeMap::new(),
            providers: BTreeMap::new(),
            trace: true,
            zdr: false,
            verify_commands: vec![],
            verify_rules: vec![],
            forbidden_paths: s(&[
                ".env", ".env.*", "*.env", "*.pem", "*.key", "*.p12", "*.pfx", "*.jks", "*.keystore", "id_rsa*", "id_ed25519*", "id_ecdsa*",
                "id_dsa*", ".npmrc", ".pypirc", ".netrc", ".git-credentials", ".htpasswd", "credentials.json", "service-account*.json", "*.tfvars",
            ]),
            forbidden_commands: vec![],
            profile: None,
            allowed_dirs: vec![],
            sandbox: "auto".into(),
            sandbox_write_extra: vec![],
            allow_private_network: false,
            subagents: vec![],
            disabled_skills: vec![],
            skill_dirs: vec![],
            model_role: "default".into(),
            auto_orient: true,
            sub_agent: false,
            auto_route: true,
            reasoning_effort: None,
            model_pinned: false,
            max_parallel_agents: 4,
            enable_parallel_tasks: true,
            auto_challenge: false,
            auto_learn: false,
            auto_scout: true,
            advisor: true,
            advisor_model: "deepseek/deepseek-v4-pro".into(),
            advisor_max_calls: 4,
            auto_review: false,
            max_reviews: 2,
            review_pipeline: true,
            review_pipeline_role: "default".into(),
            review_pipeline_tokens: 80_000,
            review_lenses: 0,
            review_backend: "melon".into(),
            review_claude_model: String::new(),
            review_run_checks: true,
            review_min_confidence: 60,
            review_repro: true,
            review_gap_pass: true,
            second_reviewer: false,
            check_references: true,
            require_read_before_edit: true,
            auto_verify: true,
            max_repairs: 2,
            verify_timeout_sec: 600,
            max_total_tokens: 300_000,
            tool_output_max_chars: 8_000,
            pricing: HashMap::new(),
            max_seconds: 1800.0,
            compact_at_tokens: 200_000,
            read_call_budget: 12,
            write_call_budget: 100,
            prune_at_tokens: 180_000,
            cache_ttl_seconds: 300,
            idle_compact_at_tokens: 150_000,
            keep_recent_tokens: 40_000,
            allowed_tools: None,
            developer_instructions: None,
            user_instructions: None,
            debug: false,
            ignored_project_keys: vec![],
            pi_mode: true,
            core_tools_only: false,
            repo_map: false,
        }
    }
}

impl Config {
    /// Build from a merged settings map. `explicit` is the set of top-level keys somebody set, so pi mode's defaults
    /// fill in only what nobody chose: a sub-agent or a profile that names its own tools or limits keeps them.
    pub fn from_map(map: serde_json::Map<String, serde_json::Value>) -> Result<Config, String> {
        let explicit: HashSet<String> = map.keys().cloned().collect();
        let mut v = serde_json::Value::Object(map);
        // remember which provider tables named api_key_env at all
        let mut set_env: Vec<String> = vec![];
        if let Some(p) = v.get("providers").and_then(|p| p.as_object()) {
            for (name, t) in p {
                if t.as_object().map_or(false, |o| o.contains_key("api_key_env")) {
                    set_env.push(name.clone());
                }
            }
        }
        // a null cwd/etc. would trip serde: drop nulls so defaults apply
        strip_nulls(&mut v);
        let mut cfg: Config = serde_json::from_value(v).map_err(|e| e.to_string())?;
        for n in set_env {
            if let Some(p) = cfg.providers.get_mut(&n) {
                p.api_key_env_set = true;
            }
        }
        for (name, m) in &cfg.mcp_servers {
            match (m.command.is_some(), m.url.is_some()) {
                (false, false) => return Err(format!("mcp_servers.{name}: MCP Server must have either 'command' (stdio) or 'url' (http/sse)")),
                (true, true) => return Err(format!("mcp_servers.{name}: MCP Server cannot have both 'command' (stdio) and 'url' (http/sse)")),
                _ => {}
            }
        }
        for h in &cfg.hooks {
            if h.command.is_none() && h.script.is_none() {
                return Err(format!("hook {}: Hook must either have 'command' or 'script'", h.name));
            }
        }
        cfg.apply_pi_mode(&explicit);
        Ok(cfg)
    }

    fn apply_pi_mode(&mut self, set: &HashSet<String>) {
        if !self.pi_mode {
            return;
        }
        let base: Vec<&str> = if self.core_tools_only {
            vec!["read_file", "write_file", "edit", "shell"]
        } else {
            // graph only shows up when graphify-out/graph.json exists
            vec!["read_file", "write_file", "edit", "shell", "grep", "glob", "list_dir", "graph", "todos", "ask_user"]
        };
        if !set.contains("allowed_tools") {
            let mut tools: Vec<String> = base.iter().map(|s| s.to_string()).collect();
            if !self.core_tools_only {
                if self.advisor && self.model_role == "default" {
                    tools.push("advisor".into());
                }
                tools.extend(self.explorer_tool());
            }
            self.allowed_tools = Some(tools);
        }
        if !set.contains("check_references") {
            self.check_references = false;
        }
        if !set.contains("auto_scout") {
            self.auto_scout = false;
        }
        if !set.contains("enable_parallel_tasks") {
            self.enable_parallel_tasks = false;
        }
        if !set.contains("tool_output_max_chars") {
            self.tool_output_max_chars = 51_200;
        }
    }

    /// The read-only investigator sub-agent (a profile's own, or the built-in one), so wide searches run in their own
    /// context and in parallel instead of filling this one. A sub-agent itself has none: no recursion.
    pub fn explorer_tool(&self) -> Vec<String> {
        if self.model_role != "default" {
            return vec![];
        }
        for d in crate::tools::subagents::default_definitions(self) {
            let readonly = d.allowed_tools.as_ref().map_or(false, |t| !t.is_empty() && t.iter().all(|x| crate::tools::subagents::READONLY_TOOLS.contains(&x.as_str())));
            if readonly && (d.name.contains("investigat") || d.name.contains("explor")) {
                return vec![format!("subagent_{}", d.name)];
            }
        }
        vec![]
    }

    pub fn api_key(&self) -> Option<String> {
        std::env::var("API_KEY").ok().filter(|s| !s.is_empty())
    }

    pub fn base_url(&self) -> String {
        std::env::var("BASE_URL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
    }

    pub fn model_name(&self) -> &str {
        &self.model.name
    }

    /// Why the model can't be called for lack of a key, or None when it can.
    pub fn missing_key(&self, model_id: Option<&str>) -> Option<String> {
        let model_id = model_id.unwrap_or(self.model_name());
        if let Some((p, _)) = crate::client::providers::split(self, model_id) {
            return if p.api_key().is_some() {
                None
            } else {
                Some(format!("No key for {}. Set {}, or run /connect {}", p.name, p.api_key_env.clone().unwrap_or_default(), p.name))
            };
        }
        if model_id.starts_with("codex/") || self.api_key().is_some() {
            return None;
        }
        Some("No API key found. Set API_KEY for the gateway, or run /connect to use a provider directly (e.g. openai, anthropic, groq, ollama)".into())
    }

    pub fn validate(&self) -> Vec<String> {
        let mut errors = vec![];
        for (name, p) in &self.providers {
            if p.base_url.is_none() && !crate::client::providers::PRESET_NAMES.contains(&name.as_str()) {
                errors.push(format!("[providers.{name}] needs a base_url (it is not one of the built-in providers)"));
            }
        }
        if !self.cwd.exists() {
            errors.push(format!("Working directory does not exist: {}", self.cwd.display()));
        }
        errors
    }
}

fn strip_nulls(v: &mut serde_json::Value) {
    if let Some(o) = v.as_object_mut() {
        o.retain(|_, x| !x.is_null());
        for x in o.values_mut() {
            strip_nulls(x);
        }
    } else if let Some(a) = v.as_array_mut() {
        for x in a {
            strip_nulls(x);
        }
    }
}

/// The config every part of a running session shares. Reads get a cheap snapshot; the agent swaps in a new one when
/// it changes something (a new working directory, a pinned model).
#[derive(Clone)]
pub struct ConfigHandle(Arc<RwLock<Arc<Config>>>);

impl ConfigHandle {
    pub fn new(c: Config) -> ConfigHandle {
        ConfigHandle(Arc::new(RwLock::new(Arc::new(c))))
    }
    pub fn get(&self) -> Arc<Config> {
        self.0.read().unwrap().clone()
    }
    pub fn update(&self, f: impl FnOnce(&mut Config)) {
        let mut guard = self.0.write().unwrap();
        let mut c = (**guard).clone();
        f(&mut c);
        *guard = Arc::new(c);
    }
    pub fn set(&self, c: Config) {
        *self.0.write().unwrap() = Arc::new(c);
    }
}
