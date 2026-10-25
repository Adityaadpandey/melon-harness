//! Sessions on disk: one JSON file per session under the data dir (0600), so `--continue` and `/resume` can pick up.
use crate::client::response::TokenUsage;
use crate::config::loader::get_data_dir;
use crate::util::paths::atomic_write;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub turn_count: usize,
    pub messages: Vec<Value>,
    pub total_usage: TokenUsage,
    /// Where it ran, so --continue can find the newest session of a folder.
    pub cwd: String,
    /// First words of the first request, to tell sessions apart.
    pub title: String,
    /// The checklist, id -> {content, done}.
    pub todos: Value,
}

impl SessionSnapshot {
    pub fn to_value(&self) -> Value {
        json!({"session_id": self.session_id, "created_at": self.created_at, "updated_at": self.updated_at, "turn_count": self.turn_count,
               "messages": self.messages, "total_usage": self.total_usage.to_value(), "cwd": self.cwd, "title": self.title, "todos": self.todos})
    }

    pub fn from_value(v: &Value) -> Option<SessionSnapshot> {
        Some(SessionSnapshot {
            session_id: v["session_id"].as_str()?.into(),
            created_at: v["created_at"].as_str().unwrap_or("").into(),
            updated_at: v["updated_at"].as_str().unwrap_or("").into(),
            turn_count: v["turn_count"].as_u64().unwrap_or(0) as usize,
            messages: v["messages"].as_array().cloned().unwrap_or_default(),
            total_usage: TokenUsage::from_value(&v["total_usage"]),
            cwd: v["cwd"].as_str().unwrap_or("").into(),
            title: v["title"].as_str().unwrap_or("").into(),
            todos: v.get("todos").cloned().unwrap_or(json!({})),
        })
    }
}

pub struct PersistenceManager {
    sessions_dir: PathBuf,
    checkpoints_dir: PathBuf,
}

impl PersistenceManager {
    pub fn new() -> PersistenceManager {
        let data = get_data_dir();
        let (s, c) = (data.join("sessions"), data.join("checkpoints"));
        for d in [&s, &c] {
            let _ = std::fs::create_dir_all(d);
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
        PersistenceManager { sessions_dir: s, checkpoints_dir: c }
    }

    fn write(&self, path: &PathBuf, snap: &SessionSnapshot) {
        let _ = atomic_write(path, &serde_json::to_string_pretty(&snap.to_value()).unwrap_or_default());
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }

    pub fn save_session(&self, snap: &SessionSnapshot) {
        self.write(&self.sessions_dir.join(format!("{}.json", snap.session_id)), snap);
    }

    pub fn load_session(&self, id: &str) -> Option<SessionSnapshot> {
        let t = std::fs::read_to_string(self.sessions_dir.join(format!("{id}.json"))).ok()?;
        SessionSnapshot::from_value(&serde_json::from_str(&t).ok()?)
    }

    /// Newest first.
    pub fn list_sessions(&self) -> Vec<Value> {
        let mut rows: Vec<Value> = std::fs::read_dir(&self.sessions_dir)
            .map(|rd| rd.flatten().filter(|e| e.path().extension().map_or(false, |x| x == "json")).filter_map(|e| std::fs::read_to_string(e.path()).ok()).filter_map(|t| serde_json::from_str::<Value>(&t).ok()).collect())
            .unwrap_or_default();
        rows.sort_by(|a, b| b["updated_at"].as_str().unwrap_or("").cmp(a["updated_at"].as_str().unwrap_or("")));
        rows.into_iter()
            .map(|d| json!({"session_id": d["session_id"], "created_at": d["created_at"], "updated_at": d["updated_at"], "turn_count": d["turn_count"], "cwd": d.get("cwd").cloned().unwrap_or(json!("")), "title": d.get("title").cloned().unwrap_or(json!(""))}))
            .collect()
    }

    /// Id of the newest saved session that ran in this folder.
    pub fn latest_for(&self, cwd: &str) -> Option<String> {
        self.list_sessions().into_iter().find(|r| r["cwd"] == cwd).and_then(|r| r["session_id"].as_str().map(String::from))
    }

    pub fn save_checkpoint(&self, snap: &SessionSnapshot) -> String {
        let id = format!("{}_{}", snap.session_id, chrono::Local::now().format("%Y%m%d_%H%M%S"));
        self.write(&self.checkpoints_dir.join(format!("{id}.json")), snap);
        id
    }

    pub fn load_checkpoint(&self, id: &str) -> Option<SessionSnapshot> {
        let t = std::fs::read_to_string(self.checkpoints_dir.join(format!("{id}.json"))).ok()?;
        SessionSnapshot::from_value(&serde_json::from_str(&t).ok()?)
    }
}
