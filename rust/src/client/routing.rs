//! Model registry (what exists, what it supports) and router (which one to use for a given job). Adding a model needs
//! no code: it shows up in /models, and a role's chain in config decides when it gets used.
use super::providers::split;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

const CATALOG_TTL: u64 = 24 * 3600;

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub context_length: usize,
    pub endpoints: Vec<String>,
    /// None = never probed. Filled from `scripts/probe_models.py` output.
    pub callable: Option<bool>,
    pub tools_ok: Option<bool>,
}

impl ModelInfo {
    /// /messages is the only route for Claude models; chat/completions is the fallback for everything else.
    pub fn endpoint(&self) -> &'static str {
        if self.endpoints.iter().any(|e| e == "/responses") && self.id.starts_with("codex/") {
            "/responses"
        } else if self.endpoints.iter().any(|e| e == "/messages") {
            "/messages"
        } else {
            "/chat/completions"
        }
    }
}

fn guess(id: &str, ctx: usize) -> ModelInfo {
    if id.starts_with("codex/") {
        // your ChatGPT login, not in the gateway catalog
        return ModelInfo { id: id.into(), context_length: 272_000, endpoints: vec!["/responses".into()], callable: None, tools_ok: None };
    }
    let eps = if id.starts_with("claude-") { "/messages" } else { "/chat/completions" };
    ModelInfo { id: id.into(), context_length: ctx, endpoints: vec![eps.into()], callable: None, tools_ok: None }
}

pub struct ModelRegistry {
    config: crate::config::ConfigHandle,
    data_dir: PathBuf,
    models: Mutex<HashMap<String, ModelInfo>>,
    order: Mutex<Vec<String>>,
    cooldown: Mutex<HashMap<String, Instant>>,
}

impl ModelRegistry {
    pub fn new(config: crate::config::ConfigHandle, data_dir: PathBuf) -> ModelRegistry {
        ModelRegistry { config, data_dir, models: Mutex::new(HashMap::new()), order: Mutex::new(vec![]), cooldown: Mutex::new(HashMap::new()) }
    }

    pub(crate) fn put(&self, info: ModelInfo) {
        let mut m = self.models.lock().unwrap();
        if !m.contains_key(&info.id) {
            self.order.lock().unwrap().push(info.id.clone());
        }
        m.insert(info.id.clone(), info);
    }

    /// Fetch the live catalog (cached for a day, usable offline).
    pub async fn load(&self) {
        let cfg = self.config.get();
        let cache = self.data_dir.join("models.json");
        let fresh = std::fs::metadata(&cache)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .map_or(false, |age| age.as_secs() < CATALOG_TTL);
        let read_cache = || std::fs::read_to_string(&cache).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let mut data: Option<Value> = if fresh { read_cache() } else { None };
        if data.is_none() {
            if let Some(key) = cfg.api_key() {
                let c = reqwest::Client::builder().timeout(Duration::from_secs(10)).build();
                if let Ok(c) = c {
                    if let Ok(r) = c.get(format!("{}/models", cfg.base_url())).bearer_auth(key).send().await {
                        if r.status().as_u16() == 200 {
                            if let Ok(v) = r.json::<Value>().await {
                                let rows = v["data"].clone();
                                let _ = std::fs::create_dir_all(&self.data_dir);
                                let _ = std::fs::write(&cache, rows.to_string());
                                data = Some(rows);
                            }
                        }
                    }
                }
            }
        }
        if data.is_none() {
            data = read_cache(); // stale beats nothing
        }
        for m in data.as_ref().and_then(Value::as_array).into_iter().flatten() {
            let Some(id) = m["id"].as_str() else { continue };
            let eps: Vec<String> = m["supported_endpoints"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect()).unwrap_or_default();
            self.put(ModelInfo {
                id: id.into(),
                context_length: m["context_length"].as_u64().filter(|n| *n > 0).unwrap_or(128_000) as usize,
                endpoints: if eps.is_empty() { vec!["/chat/completions".into()] } else { eps },
                callable: None,
                tools_ok: None,
            });
        }
        self.add_codex_models();
        self.add_provider_models();
        self.apply_probe();
    }

    fn add_provider_models(&self) {
        let Ok(rd) = std::fs::read_dir(&self.data_dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(prov) = name.strip_prefix("models-").and_then(|n| n.strip_suffix(".json")) else { continue };
            if let Some(ids) = std::fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok()) {
                self.add_provider_models_list(prov, &ids);
            }
        }
    }

    pub fn add_provider_models_list(&self, provider: &str, ids: &[String]) {
        let ctx = self.config.get().model.context_window;
        for id in ids {
            let full = format!("{provider}:{id}");
            if !self.models.lock().unwrap().contains_key(&full) {
                self.put(guess_plain(&full, ctx));
            }
        }
    }

    /// Codex models you can pick, read from the Codex CLI's own list.
    fn add_codex_models(&self) {
        let path = super::codex_auth::auth_path().parent().map(|p| p.join("models_cache.json"));
        let Some(rows) = path.and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return };
        for m in rows["models"].as_array().into_iter().flatten() {
            if m["visibility"] == "list" {
                if let Some(slug) = m["slug"].as_str() {
                    self.put(guess(&format!("codex/{slug}"), 0));
                }
            }
        }
    }

    fn apply_probe(&self) {
        let Some(rows) = std::fs::read_to_string(self.data_dir.join("model_probe.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return };
        let mut m = self.models.lock().unwrap();
        for r in rows.as_array().into_iter().flatten() {
            if let Some(info) = r["id"].as_str().and_then(|id| m.get_mut(id)) {
                info.callable = r["ok"].as_bool();
                info.tools_ok = r["tools_ok"].as_bool();
            }
        }
    }

    pub fn get(&self, id: &str) -> ModelInfo {
        // unknown to the catalog (offline, or a brand-new model): trust the id and the configured window
        self.models.lock().unwrap().get(id).cloned().unwrap_or_else(|| guess(id, self.config.get().model.context_window))
    }

    /// (known?, close matches). With no catalog (offline) anything goes.
    pub fn validate(&self, id: &str) -> (bool, Vec<String>) {
        let m = self.models.lock().unwrap();
        let cfg = self.config.get();
        if m.is_empty() || m.contains_key(id) || id.starts_with("codex/") || split(&cfg, id).is_some() {
            return (true, vec![]);
        }
        let ids: Vec<String> = self.order.lock().unwrap().clone();
        let mut near: Vec<String> = {
            let mut scored: Vec<(f32, &String)> = ids.iter().map(|i| (similar::TextDiff::from_chars(id, i.as_str()).ratio(), i)).filter(|(r, _)| *r >= 0.5).collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            scored.into_iter().take(4).map(|(_, i)| i.clone()).collect()
        };
        let tail = id.rsplit('/').next().unwrap_or(id).to_lowercase();
        for i in &ids {
            if i.to_lowercase().contains(&tail) && !near.contains(i) && near.len() < 8 {
                near.push(i.clone());
            }
        }
        near.truncate(5);
        (false, near)
    }

    pub fn all(&self) -> Vec<ModelInfo> {
        let m = self.models.lock().unwrap();
        self.order.lock().unwrap().iter().filter_map(|i| m.get(i).cloned()).collect()
    }

    pub fn mark_down(&self, id: &str, seconds: f64) {
        self.cooldown.lock().unwrap().insert(id.into(), Instant::now() + Duration::from_secs_f64(seconds));
    }

    pub fn is_cooling(&self, id: &str) -> bool {
        self.cooldown.lock().unwrap().get(id).map_or(false, |t| *t > Instant::now())
    }
}

fn guess_plain(id: &str, ctx: usize) -> ModelInfo {
    ModelInfo { id: id.into(), context_length: ctx, endpoints: vec!["/chat/completions".into()], callable: None, tools_ok: None }
}

pub struct Router {
    config: crate::config::ConfigHandle,
}

impl Router {
    pub fn new(config: crate::config::ConfigHandle) -> Router {
        Router { config }
    }

    /// Configured preference order for a role. The main model always leads the default chain, so `--model x` is honoured.
    pub fn chain(&self, role: &str) -> Vec<String> {
        let cfg = self.config.get();
        let primary = cfg.model_name().to_string();
        if role == "default" {
            let mut v = vec![primary.clone()];
            v.extend(cfg.routing.get("default").into_iter().flatten().filter(|m| **m != primary).cloned());
            return v;
        }
        if (role == "strong" || role == "debug") && cfg.model_pinned {
            return vec![primary]; // escalation never swaps the model the user picked for another
        }
        let mut chain = match cfg.routing.get(role) {
            Some(c) if !c.is_empty() => c.clone(),
            _ => self.chain("default"),
        };
        if !chain.contains(&primary) {
            chain.push(primary.clone()); // last resort for every role: the model the user chose
        }
        if role == "review" {
            // a reviewer that is the author's own model shares its blind spots
            let (mut others, own): (Vec<String>, Vec<String>) = chain.into_iter().partition(|m| *m != primary);
            others.extend(own);
            chain = others;
        }
        chain
    }

    /// (usable models in order, human-readable reasons others were skipped). Skips are never silent.
    pub fn candidates(&self, registry: &ModelRegistry, role: &str, need_tools: bool, min_context: usize) -> (Vec<String>, Vec<String>) {
        let (mut usable, mut skipped) = (vec![], vec![]);
        for (i, mid) in self.chain(role).into_iter().enumerate() {
            let info = registry.get(&mid);
            let explicit = i == 0 && role == "default";
            if info.callable == Some(false) && !explicit {
                skipped.push(format!("{mid}: not callable with this key"));
            } else if need_tools && info.tools_ok == Some(false) {
                skipped.push(format!("{mid}: no tool calling"));
            } else if min_context > 0 && info.context_length < min_context {
                skipped.push(format!("{mid}: context {} < {min_context}", info.context_length));
            } else if registry.is_cooling(&mid) {
                skipped.push(format!("{mid}: cooling down after recent failure"));
            } else {
                usable.push(mid);
            }
        }
        (usable, skipped)
    }
}
