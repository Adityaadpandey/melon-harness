use crate::config::loader::get_data_dir;
use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::paths::{atomic_write, resolve};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::any::Any;
use std::path::PathBuf;

pub struct MemoryTool {
    config: ConfigHandle,
}

impl MemoryTool {
    pub fn new(config: ConfigHandle) -> Self {
        MemoryTool { config }
    }

    /// One file per workspace, so notes about one project never show up in another.
    fn path(&self) -> PathBuf {
        let root = resolve(&self.config.get().cwd).to_string_lossy().to_string();
        get_data_dir().join("memory").join(format!("{}.json", &hex::encode(Sha256::digest(root.as_bytes()))[..16]))
    }

    pub fn load_entries(&self) -> Map<String, Value> {
        std::fs::read_to_string(self.path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["entries"].as_object().cloned()).unwrap_or_default()
    }

    fn save(&self, entries: &Map<String, Value>) {
        let p = self.path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = atomic_write(&p, &serde_json::to_string_pretty(&json!({"entries": entries})).unwrap_or_default());
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }
    fn description(&self) -> String {
        "Store and retrieve persistent memory for this workspace only. Use this to remember user preferences, important context or notes.".into()
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
                "action": {"type": "string", "description": "Action: 'set', 'get', 'delete', 'list', 'clear'"},
                "key": {"type": ["string", "null"], "description": "Memory key (required for `set`, `get`, `delete`)"},
                "value": {"type": ["string", "null"], "description": "Value to store (required for `set`)"},
            }),
            &["action"],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let action = inv.str("action").unwrap_or("").to_string();
        let key = inv.str("key").filter(|k| !k.is_empty());
        let mut entries = self.load_entries();
        match action.to_lowercase().as_str() {
            "set" => {
                let (Some(k), Some(v)) = (key, inv.str("value").filter(|v| !v.is_empty())) else { return ToolResult::error("`key` and `value` are required for 'set' action") };
                entries.insert(k.into(), json!(v));
                self.save(&entries);
                ToolResult::ok(format!("Set memory: {k}"))
            }
            "get" => {
                let Some(k) = key else { return ToolResult::error("`key` required for 'get' action") };
                match entries.get(k) {
                    None => ToolResult::ok_with(format!("Memory not found: {k}"), json!({"found": false})),
                    Some(v) => ToolResult::ok_with(format!("Memory found: {k}: {}", v.as_str().unwrap_or("")), json!({"found": true})),
                }
            }
            _ if action == "delete" => {
                let Some(k) = key else { return ToolResult::error("`key` required for 'get' action") };
                if entries.remove(k).is_none() {
                    return ToolResult::ok(format!("Memory not found: {k}"));
                }
                self.save(&entries);
                ToolResult::ok(format!("Deleted memory: {k}"))
            }
            _ if action == "list" => {
                if entries.is_empty() {
                    return ToolResult::ok_with("No memories stored", json!({"found": false}));
                }
                let mut keys: Vec<&String> = entries.keys().collect();
                keys.sort();
                let mut lines = vec!["Stored memories:".to_string()];
                lines.extend(keys.iter().map(|k| format!("  {k}: {}", entries[*k].as_str().unwrap_or(""))));
                ToolResult::ok_with(lines.join("\n"), json!({"found": true}))
            }
            _ if action == "clear" => {
                let n = entries.len();
                self.save(&Map::new());
                ToolResult::ok(format!("Cleared {n} memory entries"))
            }
            _ => ToolResult::error(format!("Unknown action: {action}")),
        }
    }
}
