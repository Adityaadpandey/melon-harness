//! MCP servers: tools served by other programs, spoken to over stdio (JSON-RPC lines) or streamable HTTP.
use crate::config::{Config, MCPServerConfig};
use crate::tools::registry::ToolRegistry;
use crate::tools::{Tool, ToolInvocation, ToolKind, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpStatus {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

impl McpStatus {
    pub fn value(&self) -> &'static str {
        match self {
            McpStatus::Disconnected => "disconnected",
            McpStatus::Connecting => "connecting",
            McpStatus::Connected => "connected",
            McpStatus::Error => "error",
        }
    }
}

#[derive(Clone, Debug)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub server_name: String,
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

enum Transport {
    Stdio { stdin: tokio::sync::Mutex<ChildStdin>, _child: Child, pending: Pending },
    Http { client: reqwest::Client, url: String, session: Mutex<Option<String>> },
}

pub struct McpClient {
    pub name: String,
    config: MCPServerConfig,
    cwd: std::path::PathBuf,
    pub status: Mutex<McpStatus>,
    transport: tokio::sync::Mutex<Option<Arc<Transport>>>,
    next_id: std::sync::atomic::AtomicI64,
    pub tools: Mutex<Vec<McpToolInfo>>,
}

impl McpClient {
    pub fn new(name: &str, config: MCPServerConfig, cwd: &std::path::Path) -> McpClient {
        McpClient {
            name: name.into(),
            config,
            cwd: cwd.to_path_buf(),
            status: Mutex::new(McpStatus::Disconnected),
            transport: tokio::sync::Mutex::new(None),
            next_id: std::sync::atomic::AtomicI64::new(1),
            tools: Mutex::new(vec![]),
        }
    }

    pub fn status(&self) -> McpStatus {
        *self.status.lock().unwrap()
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let t = self.transport.lock().await.clone().ok_or_else(|| format!("Not connected to server {}", self.name))?;
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let resp = match &*t {
            Transport::Stdio { stdin, pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().unwrap().insert(id, tx);
                stdin.lock().await.write_all(format!("{msg}\n").as_bytes()).await.map_err(|e| e.to_string())?;
                tokio::time::timeout(Duration::from_secs(120), rx).await.map_err(|_| "MCP request timed out".to_string())?.map_err(|_| "MCP server closed".to_string())?
            }
            Transport::Http { client, url, session } => {
                let mut rb = client.post(url).header("Accept", "application/json, text/event-stream").json(&msg);
                if let Some(s) = session.lock().unwrap().clone() {
                    rb = rb.header("Mcp-Session-Id", s);
                }
                let r = rb.send().await.map_err(|e| e.to_string())?;
                if let Some(s) = r.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
                    *session.lock().unwrap() = Some(s.to_string());
                }
                let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                let body = r.text().await.map_err(|e| e.to_string())?;
                if ct.contains("text/event-stream") {
                    body.lines().filter_map(|l| l.strip_prefix("data:")).filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok()).find(|v| v["id"] == id).ok_or("no response in event stream")?
                } else {
                    serde_json::from_str(&body).map_err(|e| e.to_string())?
                }
            }
        };
        if let Some(e) = resp.get("error") {
            return Err(e["message"].as_str().unwrap_or("MCP error").to_string());
        }
        Ok(resp["result"].clone())
    }

    async fn notify(&self, method: &str) {
        let Some(t) = self.transport.lock().await.clone() else { return };
        let msg = json!({"jsonrpc": "2.0", "method": method});
        match &*t {
            Transport::Stdio { stdin, .. } => {
                let _ = stdin.lock().await.write_all(format!("{msg}\n").as_bytes()).await;
            }
            Transport::Http { client, url, session } => {
                let mut rb = client.post(url).header("Accept", "application/json, text/event-stream").json(&msg);
                if let Some(s) = session.lock().unwrap().clone() {
                    rb = rb.header("Mcp-Session-Id", s);
                }
                let _ = rb.send().await;
            }
        }
    }

    pub async fn connect(&self) -> Result<(), String> {
        if self.status() == McpStatus::Connected {
            return Ok(());
        }
        *self.status.lock().unwrap() = McpStatus::Connecting;
        let res = async {
            let transport = if let Some(cmd) = &self.config.command {
                let mut child = Command::new(cmd)
                    .args(&self.config.args)
                    .envs(&self.config.env)
                    .current_dir(self.config.cwd.clone().unwrap_or_else(|| self.cwd.clone()))
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| e.to_string())?;
                let stdin = child.stdin.take().ok_or("no stdin")?;
                let stdout = child.stdout.take().ok_or("no stdout")?;
                let pending: Pending = Default::default();
                let p2 = pending.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if let Ok(v) = serde_json::from_str::<Value>(&line) {
                            if let Some(id) = v["id"].as_i64() {
                                if let Some(tx) = p2.lock().unwrap().remove(&id) {
                                    let _ = tx.send(v);
                                }
                            }
                        }
                    }
                    p2.lock().unwrap().clear();
                });
                Transport::Stdio { stdin: tokio::sync::Mutex::new(stdin), _child: child, pending }
            } else {
                Transport::Http { client: reqwest::Client::new(), url: self.config.url.clone().unwrap_or_default(), session: Mutex::new(None) }
            };
            *self.transport.lock().await = Some(Arc::new(transport));
            self.rpc("initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "melon", "version": "1.0"}})).await?;
            self.notify("notifications/initialized").await;
            let listed = self.rpc("tools/list", json!({})).await?;
            let tools: Vec<McpToolInfo> = listed["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| McpToolInfo { name: t["name"].as_str().unwrap_or("").into(), description: t["description"].as_str().unwrap_or("").into(), input_schema: t.get("inputSchema").cloned().unwrap_or(json!({})), server_name: self.name.clone() })
                .collect();
            *self.tools.lock().unwrap() = tools;
            Ok::<(), String>(())
        }
        .await;
        *self.status.lock().unwrap() = if res.is_ok() { McpStatus::Connected } else { McpStatus::Error };
        res
    }

    pub async fn disconnect(&self) {
        *self.transport.lock().await = None;
        self.tools.lock().unwrap().clear();
        *self.status.lock().unwrap() = McpStatus::Disconnected;
    }

    pub async fn call_tool(&self, name: &str, arguments: &Value) -> Result<(String, bool), String> {
        if self.status() != McpStatus::Connected {
            return Err(format!("Not connected to server {}", self.name));
        }
        let r = self.rpc("tools/call", json!({"name": name, "arguments": arguments})).await?;
        let out: Vec<String> = r["content"].as_array().into_iter().flatten().map(|c| c["text"].as_str().map(String::from).unwrap_or_else(|| c.to_string())).collect();
        Ok((out.join("\n"), r["isError"].as_bool().unwrap_or(false)))
    }
}

pub struct McpTool {
    info: McpToolInfo,
    client: Arc<McpClient>,
    name: String,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> String {
        self.info.description.clone()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Mcp
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn is_mutating(&self, _p: &Value) -> bool {
        true
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": self.info.input_schema.get("properties").cloned().unwrap_or(json!({})), "required": self.info.input_schema.get("required").cloned().unwrap_or(json!([]))})
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        match self.client.call_tool(&self.info.name, &inv.params).await {
            Ok((out, true)) => ToolResult::error(out),
            Ok((out, false)) => ToolResult::ok(out),
            Err(e) => ToolResult::error(format!("MCP tool failed: {e}")),
        }
    }
}

pub struct McpManager {
    config: crate::config::ConfigHandle,
    clients: Mutex<Vec<Arc<McpClient>>>,
    initialized: std::sync::atomic::AtomicBool,
}

impl McpManager {
    pub fn new(config: crate::config::ConfigHandle) -> McpManager {
        McpManager { config, clients: Mutex::new(vec![]), initialized: Default::default() }
    }

    pub async fn initialize(&self) {
        use std::sync::atomic::Ordering;
        let cfg: Arc<Config> = self.config.get();
        if self.initialized.load(Ordering::SeqCst) || cfg.mcp_servers.is_empty() {
            return;
        }
        let clients: Vec<Arc<McpClient>> = cfg.mcp_servers.iter().filter(|(_, c)| c.enabled).map(|(n, c)| Arc::new(McpClient::new(n, c.clone(), &cfg.cwd))).collect();
        let futs = clients.iter().map(|c| {
            let c = c.clone();
            let t = Duration::from_secs_f64(c.config.startup_timeout_sec);
            async move {
                let _ = tokio::time::timeout(t, c.connect()).await;
            }
        });
        futures::future::join_all(futs).await;
        *self.clients.lock().unwrap() = clients;
        self.initialized.store(true, Ordering::SeqCst);
    }

    pub fn register_tools(&self, registry: &ToolRegistry) -> usize {
        let mut n = 0;
        for c in self.clients.lock().unwrap().iter().filter(|c| c.status() == McpStatus::Connected) {
            for info in c.tools.lock().unwrap().iter() {
                registry.register_mcp_tool(Arc::new(McpTool { name: format!("{}__{}", c.name, info.name), info: info.clone(), client: c.clone() }));
                n += 1;
            }
        }
        n
    }

    pub async fn shutdown(&self) {
        let clients = std::mem::take(&mut *self.clients.lock().unwrap());
        for c in clients {
            c.disconnect().await;
        }
        self.initialized.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn get_all_servers(&self) -> Vec<Value> {
        self.clients.lock().unwrap().iter().map(|c| json!({"name": c.name, "status": c.status().value(), "tools": c.tools.lock().unwrap().len()})).collect()
    }
}
