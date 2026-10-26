use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::re::lit;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::time::Duration;

pub struct WebSearchTool;

fn strip_tags(s: &str) -> String {
    let t = lit(r"<[^>]*>").replace_all(s, "").to_string();
    t.replace("&amp;", "&").replace("&quot;", "\"").replace("&#x27;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").trim().to_string()
}

/// DuckDuckGo wraps result links as //duckduckgo.com/l/?uddg=<url-encoded target>.
fn real_url(href: &str) -> String {
    let full = if href.starts_with("//") { format!("https:{href}") } else { href.to_string() };
    if let Ok(u) = reqwest::Url::parse(&full) {
        if let Some((_, v)) = u.query_pairs().find(|(k, _)| k == "uddg") {
            return v.to_string();
        }
    }
    full
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }
    fn description(&self) -> String {
        "Search the web; returns titles, URLs and snippets. Use it for recent or niche facts that the repo and your memory cannot settle, then fetch the best result instead of trusting a snippet. Include the current year in queries about latest versions.".into()
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
                "query": {"type": "string", "description": "Search query"},
                "max_results": {"type": "integer", "minimum": 1, "maximum": 20, "default": 10, "description": "Maximum results to return (default: 10)"},
            }),
            &["query"],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let query = inv.str("query").unwrap_or("").to_string();
        let max = inv.params["max_results"].as_u64().unwrap_or(10) as usize;
        let client = match reqwest::Client::builder().timeout(Duration::from_secs(20)).user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120 Safari/537.36").build() {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Search failed: {e}")),
        };
        let body = match client.post("https://html.duckduckgo.com/html/").form(&[("q", query.as_str()), ("kl", "us-en"), ("df", "y")]).send().await {
            Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
            Ok(r) => return ToolResult::error(format!("Search failed: HTTP {}", r.status().as_u16())),
            Err(e) => return ToolResult::error(format!("Search failed: {e}")),
        };
        let link = lit(r#"(?s)<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#);
        let snip = lit(r#"(?s)<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#);
        let mut results: Vec<(String, String, String)> = vec![];
        let mut at = 0;
        while results.len() < max && at < body.len() {
            let Some(c) = link.captures(&body[at..]) else { break };
            let (_, e) = link.find(&body[at..]).unwrap();
            let after = &body[at + e..];
            // the snippet belongs to this result only if it comes before the next link
            let next_link = link.find(after).map_or(after.len(), |m| m.0);
            let snippet = snip.captures(&after[..next_link]).and_then(|s| s[1].clone()).map(|s| strip_tags(&s)).unwrap_or_default();
            results.push((strip_tags(&c[2].clone().unwrap_or_default()), real_url(&c[1].clone().unwrap_or_default()), snippet));
            at += e.max(1);
        }
        if results.is_empty() {
            return ToolResult::ok_with(format!("No results found for: {query}"), json!({"results": 0}));
        }
        let mut lines = vec![format!("Search results for: {query}")];
        for (i, (t, u, s)) in results.iter().enumerate() {
            lines.push(format!("{}. Title: {t}", i + 1));
            lines.push(format!("   URL: {u}"));
            if !s.is_empty() {
                lines.push(format!("   Snippet: {s}"));
            }
            lines.push(String::new());
        }
        ToolResult::ok_with(lines.join("\n"), json!({"results": results.len()}))
    }
}
