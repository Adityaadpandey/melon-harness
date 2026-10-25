//! Talk to model APIs directly, without the gateway. `-m groq:llama-3.3-70b` sends the request straight to Groq using
//! GROQ_API_KEY. A model id without a known `name:` prefix still goes through the gateway (API_KEY / BASE_URL).
//! Almost every model API speaks one of two wire formats, OpenAI chat or Anthropic messages, so a provider is only a
//! URL, the env var holding its key, and which format it speaks.
use crate::config::Config;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Provider {
    pub name: String,
    pub base_url: String,
    /// None = a local server that ignores keys.
    pub api_key_env: Option<String>,
    pub protocol: String,
    pub headers: BTreeMap<String, String>,
}

impl Provider {
    pub fn api_key(&self) -> Option<String> {
        match &self.api_key_env {
            // the clients refuse an empty key even when the server never checks it
            None => Some("local".into()),
            Some(env) => std::env::var(env).ok().filter(|s| !s.is_empty()),
        }
    }
}

const PRESET_TABLE: &[(&str, &str, Option<&str>, &str)] = &[
    ("openai", "https://api.openai.com/v1", Some("OPENAI_API_KEY"), "openai"),
    ("anthropic", "https://api.anthropic.com", Some("ANTHROPIC_API_KEY"), "anthropic"),
    ("gemini", "https://generativelanguage.googleapis.com/v1beta/openai", Some("GEMINI_API_KEY"), "openai"),
    ("openrouter", "https://openrouter.ai/api/v1", Some("OPENROUTER_API_KEY"), "openai"),
    ("groq", "https://api.groq.com/openai/v1", Some("GROQ_API_KEY"), "openai"),
    ("deepseek", "https://api.deepseek.com/v1", Some("DEEPSEEK_API_KEY"), "openai"),
    ("mistral", "https://api.mistral.ai/v1", Some("MISTRAL_API_KEY"), "openai"),
    ("xai", "https://api.x.ai/v1", Some("XAI_API_KEY"), "openai"),
    ("together", "https://api.together.xyz/v1", Some("TOGETHER_API_KEY"), "openai"),
    ("fireworks", "https://api.fireworks.ai/inference/v1", Some("FIREWORKS_API_KEY"), "openai"),
    ("cerebras", "https://api.cerebras.ai/v1", Some("CEREBRAS_API_KEY"), "openai"),
    ("moonshot", "https://api.moonshot.ai/v1", Some("MOONSHOT_API_KEY"), "openai"),
    ("zai", "https://api.z.ai/api/paas/v4", Some("ZAI_API_KEY"), "openai"),
    ("ollama", "http://localhost:11434/v1", None, "openai"),
    ("lmstudio", "http://localhost:1234/v1", None, "openai"),
];

pub const PRESET_NAMES: &[&str] =
    &["openai", "anthropic", "gemini", "openrouter", "groq", "deepseek", "mistral", "xai", "together", "fireworks", "cerebras", "moonshot", "zai", "ollama", "lmstudio"];

pub fn preset(name: &str) -> Option<Provider> {
    PRESET_TABLE.iter().find(|p| p.0 == name).map(|p| Provider {
        name: p.0.into(),
        base_url: p.1.into(),
        api_key_env: p.2.map(String::from),
        protocol: p.3.into(),
        headers: BTreeMap::new(),
    })
}

/// Built-ins with the user's [providers.*] tables laid over them.
pub fn all_providers(config: &Config) -> BTreeMap<String, Provider> {
    let mut out: BTreeMap<String, Provider> = PRESET_TABLE.iter().filter_map(|p| preset(p.0).map(|x| (p.0.to_string(), x))).collect();
    for (name, c) in &config.providers {
        let base = preset(name);
        let Some(url) = c.base_url.clone().or_else(|| base.as_ref().map(|b| b.base_url.clone())) else { continue };
        let env = if c.api_key_env_set || c.api_key_env.is_some() { c.api_key_env.clone() } else { base.as_ref().and_then(|b| b.api_key_env.clone()) };
        out.insert(
            name.clone(),
            Provider {
                name: name.clone(),
                base_url: url,
                api_key_env: env,
                protocol: c.protocol.clone().or_else(|| base.as_ref().map(|b| b.protocol.clone())).unwrap_or_else(|| "openai".into()),
                headers: c.headers.clone(),
            },
        );
    }
    out
}

/// `groq:llama-3.3-70b` -> (groq, "llama-3.3-70b"). Only the first colon counts, so `ollama:qwen3:8b` keeps its tag.
/// None when the prefix isn't a provider.
pub fn split(config: &Config, model_id: &str) -> Option<(Provider, String)> {
    let (name, rest) = model_id.split_once(':')?;
    if rest.is_empty() {
        return None;
    }
    all_providers(config).remove(name).map(|p| (p, rest.to_string()))
}

/// The provider's own model list, which also proves the key works.
pub async fn fetch_models(p: &Provider) -> Result<Vec<String>, String> {
    let root = p.base_url.trim_end_matches('/').to_string();
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build().map_err(|e| e.to_string())?;
    let mut req;
    if p.protocol == "anthropic" {
        let url = if root.ends_with("/v1") { format!("{root}/models") } else { format!("{root}/v1/models") };
        req = client.get(url).header("x-api-key", p.api_key().unwrap_or_default()).header("anthropic-version", "2023-06-01");
    } else {
        req = client.get(format!("{root}/models")).bearer_auth(p.api_key().unwrap_or_default());
    }
    for (k, v) in &p.headers {
        req = req.header(k, v);
    }
    let r = req.send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status()));
    }
    let v: serde_json::Value = r.json().await.map_err(|e| e.to_string())?;
    let mut ids: Vec<String> = v["data"].as_array().map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default();
    ids.sort();
    Ok(ids)
}
