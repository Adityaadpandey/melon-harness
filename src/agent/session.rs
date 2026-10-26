//! One running conversation: the client, the tools, the context, the approval policy and the trace, wired together.
use crate::agent::skills::{load_skills, Skill};
use crate::client::llm_client::LLMClient;
use crate::config::loader::get_data_dir;
use crate::config::{ConfigHandle, HookTrigger};
use crate::context::compaction::ChatCompactor;
use crate::context::loop_detector::LoopDetector;
use crate::context::ContextManager;
use crate::hooks::HookSystem;
use crate::prompts::get_system_prompt;
use crate::safety::approval::ApprovalManager;
use crate::tools::builtin::advisor::AdvisorTool;
use crate::tools::builtin::memory::MemoryTool;
use crate::tools::builtin::recall::RecallResultTool;
use crate::tools::mcp::McpManager;
use crate::tools::registry::{create_default_registry, ToolRegistry};
use crate::util::archive::ResultStore;
use crate::util::trace::Tracer;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct Session {
    pub config: ConfigHandle,
    session_id: Mutex<String>,
    pub tracer: Arc<Tracer>,
    pub client: Arc<LLMClient>,
    pub tool_registry: Arc<ToolRegistry>,
    pub skills: Vec<Skill>,
    pub context_manager: Arc<Mutex<ContextManager>>,
    pub mcp_manager: McpManager,
    pub chat_compactor: ChatCompactor,
    pub approval_manager: Arc<ApprovalManager>,
    pub loop_detector: Mutex<LoopDetector>,
    pub hook_system: HookSystem,
    pub created_at: Mutex<String>,
    pub updated_at: Mutex<String>,
    pub turn_count: AtomicUsize,
}

impl Session {
    pub async fn create(config: ConfigHandle) -> Arc<Session> {
        let cfg0 = config.get();
        let session_id = uuid::Uuid::new_v4().to_string();
        let tracer = Arc::new(Tracer::new(&session_id, cfg0.trace.then(|| get_data_dir().join("traces"))));
        let client = Arc::new(LLMClient::new(config.clone(), tracer.clone()));
        let tool_registry = create_default_registry(config.clone());
        *tool_registry.tracer.lock().unwrap() = Some(tracer.clone());
        let archive = Arc::new(ResultStore::new(Some(get_data_dir().join("results").join(&session_id))));
        *tool_registry.archive.lock().unwrap() = archive.clone();
        if let Some(t) = tool_registry.get("recall_result") {
            if let Some(r) = t.as_any().downcast_ref::<RecallResultTool>() {
                *r.store.lock().unwrap() = archive.clone();
            }
        }
        if cfg0.advisor && !cfg0.routing.contains_key("advisor") {
            let m = cfg0.advisor_model.clone();
            config.update(|c| {
                c.routing.insert("advisor".into(), vec![m]);
            });
        }
        let skills = load_skills(&config.get());

        client.initialize().await;
        let mcp = McpManager::new(config.clone());
        mcp.initialize().await;
        mcp.register_tools(&tool_registry);

        let cfg = config.get();
        let user_memory = load_memory(&MemoryTool::new(config.clone()));
        let system_prompt = get_system_prompt(&cfg, user_memory.as_deref(), &tool_registry.get_tools(), &skills);
        let cm = Arc::new(Mutex::new(ContextManager::new(config.clone(), system_prompt)));
        cm.lock().unwrap().archive = Some(archive);
        {
            let cm2 = cm.clone();
            tool_registry.set_is_live(Arc::new(move |id| cm2.lock().unwrap().has_live_result(id)));
        }
        if let Some(t) = tool_registry.get("advisor") {
            if let Some(a) = t.as_any().downcast_ref::<AdvisorTool>() {
                *a.client.lock().unwrap() = Some(client.clone());
                let cm3 = cm.clone();
                *a.transcript.lock().unwrap() = Some(Arc::new(move || cm3.lock().unwrap().get_messages()));
            }
        }

        let verify: Vec<String> = cfg.verify_commands.iter().cloned().chain(cfg.verify_rules.iter().flat_map(|r| r.commands.iter().filter(|c| !c.contains('{')).cloned())).collect();
        let approval = Arc::new(ApprovalManager::new(cfg.approval, &cfg.cwd, &verify));
        let hook_system = HookSystem::new(&cfg);
        hook_system.fire(HookTrigger::SessionStart, &[("session_id", session_id.clone())]).await;
        if !cfg.sub_agent && cfg.model_role == "default" {
            crate::util::rank::warm(cfg.cwd.clone(), cfg.forbidden_paths.clone()); // first search of the session finds the index ready
        }
        Arc::new(Session {
            chat_compactor: ChatCompactor::new(client.clone()),
            config,
            session_id: Mutex::new(session_id),
            tracer,
            client,
            tool_registry,
            skills,
            context_manager: cm,
            mcp_manager: mcp,
            approval_manager: approval,
            loop_detector: Mutex::new(LoopDetector::default()),
            hook_system,
            created_at: Mutex::new(chrono::Local::now().to_rfc3339()),
            updated_at: Mutex::new(chrono::Local::now().to_rfc3339()),
            turn_count: AtomicUsize::new(0),
        })
    }

    pub fn session_id(&self) -> String {
        self.session_id.lock().unwrap().clone()
    }

    /// Resume swaps in the saved id: the trace and the archived results follow it, so one session's data stays in one
    /// place (traces/<id>.jsonl, results/<id>/) instead of under a throwaway id.
    pub fn set_session_id(&self, id: &str) {
        *self.session_id.lock().unwrap() = id.to_string();
        self.tracer.rename(id);
        self.tool_registry.archive.lock().unwrap().move_to(&get_data_dir().join("results").join(id));
    }

    pub fn increment_turn(&self) -> usize {
        *self.updated_at.lock().unwrap() = chrono::Local::now().to_rfc3339();
        self.turn_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn set_turns(&self, n: usize) {
        self.turn_count.store(n, Ordering::SeqCst);
    }

    pub fn turns(&self) -> usize {
        self.turn_count.load(Ordering::SeqCst)
    }

    pub fn get_stats(&self) -> Value {
        let cm = self.context_manager.lock().unwrap();
        serde_json::json!({
            "session_id": self.session_id(), "created_at": self.created_at.lock().unwrap().clone(), "turn_count": self.turns(),
            "message_count": cm.message_count(), "token_usage": cm.total_usage.to_value(),
            "tools_count": self.tool_registry.get_tools().len(), "mcp_servers": self.tool_registry.connected_mcp_tools().len(),
        })
    }
}

/// The memory tool saves one file per workspace; the old shared user_memory.json is still read so nothing stored earlier is lost.
pub(crate) fn load_memory(tool: &MemoryTool) -> Option<String> {
    let mut entries = tool.load_entries();
    let legacy = get_data_dir().join("user_memory.json");
    if let Some(old) = std::fs::read_to_string(legacy).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["entries"].as_object().cloned()) {
        let mut merged = old;
        merged.extend(entries);
        entries = merged;
    }
    if entries.is_empty() {
        return None;
    }
    let mut lines = vec!["User preferences and notes:".to_string()];
    for (k, v) in &entries {
        lines.push(format!("- {k}: {}", v.as_str().unwrap_or("")));
    }
    Some(lines.join("\n"))
}
