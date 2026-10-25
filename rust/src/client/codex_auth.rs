//! Reuses the ChatGPT login that the Codex CLI already stored, so melon can call Codex models with your subscription
//! instead of an API key. We only read (and refresh) ~/.codex/auth.json; we never ask you to log in again.
use base64::Engine;
use serde_json::{json, Value};
use std::path::PathBuf;

/// The public client id the Codex CLI itself uses for token refresh.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";

pub fn auth_path() -> PathBuf {
    std::env::var("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".codex")).join("auth.json")
}

/// exp claim of a JWT, or 0 if it can't be read (treated as expired).
fn expiry(jwt: &str) -> f64 {
    let body = jwt.split('.').nth(1).unwrap_or("");
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.trim_end_matches('='))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["exp"].as_f64())
        .unwrap_or(0.0)
}

pub struct CodexAuth {
    pub path: PathBuf,
}

impl Default for CodexAuth {
    fn default() -> Self {
        CodexAuth { path: auth_path() }
    }
}

impl CodexAuth {
    fn load(&self) -> Result<Value, String> {
        let data: Value = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .ok_or_else(|| format!("No Codex login at {}. Run `codex login` first.", self.path.display()))?;
        if data["tokens"]["access_token"].as_str().map_or(true, str::is_empty) {
            return Err("Codex is logged in with an API key, not ChatGPT. Run `codex login`.".into());
        }
        Ok(data)
    }

    async fn refresh(&self, mut data: Value) -> Result<Value, String> {
        let refresh_token = data["tokens"]["refresh_token"].as_str().unwrap_or("").to_string();
        let r = reqwest::Client::new()
            .post(REFRESH_URL)
            .timeout(std::time::Duration::from_secs(20))
            .json(&json!({"client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": refresh_token, "scope": "openid profile email"}))
            .send()
            .await
            .map_err(|e| format!("Connection error while refreshing the Codex login: {e}"))?;
        if r.status().as_u16() != 200 {
            return Err(format!("Codex token refresh failed ({}). Run `codex login` again.", r.status().as_u16()));
        }
        let new: Value = r.json().await.map_err(|e| e.to_string())?;
        for k in ["access_token", "refresh_token", "id_token"] {
            if let Some(v) = new.get(k).filter(|v| v.as_str().map_or(false, |s| !s.is_empty())) {
                data["tokens"][k] = v.clone();
            }
        }
        data["last_refresh"] = json!(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
        // written back so the Codex CLI keeps working with the rotated refresh token
        let _ = std::fs::write(&self.path, serde_json::to_string_pretty(&data).unwrap_or_default());
        Ok(data)
    }

    /// (access_token, account_id), refreshed first if it is about to expire.
    pub async fn credentials(&self, force_refresh: bool) -> Result<(String, String), String> {
        let mut data = self.load()?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        if force_refresh || expiry(data["tokens"]["access_token"].as_str().unwrap_or("")) < now + 60.0 {
            data = self.refresh(data).await?;
        }
        let t = &data["tokens"];
        Ok((t["access_token"].as_str().unwrap_or("").to_string(), t["account_id"].as_str().unwrap_or("").to_string()))
    }
}
