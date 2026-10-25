use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::netguard::check_url;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::time::Duration;

pub struct WebFetchTool {
    config: ConfigHandle,
}
impl WebFetchTool {
    pub fn new(config: ConfigHandle) -> Self {
        WebFetchTool { config }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn description(&self) -> String {
        "Fetch a URL and return the body as text. Use it for URLs the user gave or that a search returned; do not guess URLs from memory. It cannot reach pages behind a login: use `gh` or an authenticated tool for those.".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> Value {
        schema_of(
            json!({
                "url": {"type": "string", "description": "URL to fetch (must be http:// or https://)"},
                "timeout": {"type": "integer", "minimum": 5, "maximum": 120, "default": 30, "description": "Request timeout in seconds (default: 120)"},
            }),
            &["url"],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let mut url = inv.str("url").unwrap_or("").to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return ToolResult::error("Url must be http:// or https://");
        }
        let allow = self.config.get().allow_private_network;
        let timeout = inv.params["timeout"].as_u64().unwrap_or(30);
        let client = match reqwest::Client::builder().timeout(Duration::from_secs(timeout)).redirect(reqwest::redirect::Policy::none()).build() {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Request failed: {e}")),
        };
        let mut resp = None;
        for _ in 0..6 {
            // every hop is checked, so a public page cannot redirect us to localhost or cloud metadata
            if let Some(why) = check_url(&url, allow) {
                return ToolResult::error(format!("Blocked: {why}"));
            }
            let r = match client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => return ToolResult::error(format!("Request failed: {e}")),
            };
            if r.status().is_redirection() {
                if let Some(loc) = r.headers().get("location").and_then(|l| l.to_str().ok()) {
                    url = match reqwest::Url::parse(&url).and_then(|u| u.join(loc)) {
                        Ok(u) => u.to_string(),
                        Err(e) => return ToolResult::error(format!("Request failed: {e}")),
                    };
                    continue;
                }
            }
            resp = Some(r);
            break;
        }
        let Some(r) = resp else { return ToolResult::error("Too many redirects") };
        let status = r.status();
        if !status.is_success() {
            return ToolResult::error(format!("HTTP {}: {}", status.as_u16(), status.canonical_reason().unwrap_or("")));
        }
        let bytes = match r.bytes().await {
            Ok(b) => b,
            Err(e) => return ToolResult::error(format!("Request failed: {e}")),
        };
        let mut text = String::from_utf8_lossy(&bytes).to_string();
        if text.len() > 100 * 1024 {
            text = format!("{}\n... [content truncated]", crate::util::text::clip(&text, 100 * 1024));
        }
        ToolResult::ok_with(text, json!({"status_code": status.as_u16(), "content_length": bytes.len()}))
    }
}
