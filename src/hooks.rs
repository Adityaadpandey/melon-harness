//! User-configured shell hooks around the agent's life: before/after a request, a tool, compaction, errors, stop.
use crate::config::{Config, HookConfig, HookTrigger};
use crate::tools::ToolResult;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

pub struct HookSystem {
    cwd: std::path::PathBuf,
    hooks: Vec<HookConfig>,
}

impl HookSystem {
    pub fn new(config: &Config) -> HookSystem {
        let hooks = if config.hooks_enabled { config.hooks.iter().filter(|h| h.enabled).cloned().collect() } else { vec![] };
        HookSystem { cwd: config.cwd.clone(), hooks }
    }

    /// Run one hook and return (exit_code, stderr). A hook that cannot run or times out counts as exit 1: it never blocks.
    async fn run_hook(&self, hook: &HookConfig, env: &HashMap<String, String>) -> (i32, String) {
        let timeout = Duration::from_secs_f64(hook.timeout_sec);
        let run = async {
            if let Some(c) = &hook.command {
                return self.run_command(c, timeout, env).await;
            }
            let script = hook.script.clone().unwrap_or_default();
            let dir = tempfile::tempdir().ok()?;
            let path = dir.path().join("hook.sh");
            std::fs::write(&path, format!("#!/bin/bash\n{script}")).ok()?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).ok()?;
            self.run_command(&path.to_string_lossy(), timeout, env).await
        };
        run.await.unwrap_or((1, String::new()))
    }

    async fn run_command(&self, command: &str, timeout: Duration, env: &HashMap<String, String>) -> Option<(i32, String)> {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(command).current_dir(&self.cwd).envs(env).stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null()).kill_on_drop(true);
        let mut child = crate::util::detach::detach(&mut cmd).spawn().ok()?;
        let pid = child.id();
        let mut err = child.stderr.take()?;
        let wait = async {
            let mut buf = Vec::new();
            use tokio::io::AsyncReadExt;
            let _ = err.read_to_end(&mut buf).await;
            let st = child.wait().await.ok()?;
            Some((st.code().unwrap_or(1), String::from_utf8_lossy(&buf).trim().to_string()))
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(r) => r,
            Err(_) => {
                if let Some(p) = pid {
                    unsafe { libc::killpg(p as i32, libc::SIGKILL) };
                }
                Some((1, String::new()))
            }
        }
    }

    fn matching<'a>(&'a self, trigger: HookTrigger, tool: Option<&'a str>) -> impl Iterator<Item = &'a HookConfig> {
        self.hooks.iter().filter(move |h| {
            if h.trigger != trigger {
                return false;
            }
            // matcher is a regex on the tool name, like Claude Code's: "edit|write_file"
            match (&h.matcher, tool) {
                (Some(m), Some(t)) if !m.is_empty() => crate::util::re::Re::new(&format!("^(?:{m})$"), false).map_or(false, |r| r.is_match(t)),
                _ => true,
            }
        })
    }

    fn build_env(&self, trigger: HookTrigger, tool: Option<&str>, user_message: Option<&str>) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.insert("AI_AGENT_TRIGGER".into(), trigger.value().into());
        env.insert("AI_AGENT_CWD".into(), self.cwd.to_string_lossy().to_string());
        if let Some(t) = tool.filter(|t| !t.is_empty()) {
            env.insert("AI_AGENT_TOOL_NAME".into(), t.into());
        }
        if let Some(m) = user_message.filter(|m| !m.is_empty()) {
            env.insert("AI_AGENT_USER_MESSAGE".into(), m.into());
        }
        env
    }

    pub async fn trigger_before_agent(&self, user_message: &str) {
        let env = self.build_env(HookTrigger::BeforeAgent, None, Some(user_message));
        for h in self.hooks.iter().filter(|h| h.trigger == HookTrigger::BeforeAgent) {
            self.run_hook(h, &env).await;
        }
    }

    pub async fn trigger_after_agent(&self, user_message: &str, agent_response: &str) {
        let mut env = self.build_env(HookTrigger::AfterAgent, None, Some(user_message));
        env.insert("AI_AGENT_RESPONSE".into(), agent_response.into());
        for h in self.hooks.iter().filter(|h| h.trigger == HookTrigger::AfterAgent) {
            self.run_hook(h, &env).await;
        }
    }

    /// Exit code 2 from a before_tool hook blocks the call; its stderr is the reason the model sees.
    pub async fn trigger_before_tool(&self, tool: &str, params: &Value) -> Option<String> {
        let mut env = self.build_env(HookTrigger::BeforeTool, Some(tool), None);
        env.insert("AI_AGENT_TOOL_PARAMS".into(), params.to_string());
        for h in self.matching(HookTrigger::BeforeTool, Some(tool)) {
            let (code, err) = self.run_hook(h, &env).await;
            if code == 2 {
                return Some(format!("Blocked by hook '{}': {}", h.name, if err.is_empty() { "no reason given" } else { &err }));
            }
        }
        None
    }

    /// Exit code 2 from an after_tool hook (a linter that found problems) hands its stderr back to the model.
    pub async fn trigger_after_tool(&self, tool: &str, params: &Value, result: &ToolResult) -> Option<String> {
        let mut env = self.build_env(HookTrigger::AfterTool, Some(tool), None);
        env.insert("AI_AGENT_TOOL_PARAMS".into(), params.to_string());
        env.insert("AI_AGENT_TOOL_RESULT".into(), result.to_model_output());
        let mut feedback = vec![];
        for h in self.matching(HookTrigger::AfterTool, Some(tool)) {
            let (code, err) = self.run_hook(h, &env).await;
            if code == 2 && !err.is_empty() {
                feedback.push(format!("[hook '{}']: {err}", h.name));
            }
        }
        (!feedback.is_empty()).then(|| feedback.join("\n"))
    }

    /// Events with no veto (compaction, errors): run every matching hook, ignore the exit code.
    /// `info` becomes AI_AGENT_<KEY> env vars, e.g. fire(PreCompact, [("tokens", "180000")]).
    pub async fn fire(&self, trigger: HookTrigger, info: &[(&str, String)]) {
        let mut env = self.build_env(trigger, None, None);
        for (k, v) in info {
            env.insert(format!("AI_AGENT_{}", k.to_uppercase()), v.clone());
        }
        for h in self.matching(trigger, None) {
            self.run_hook(h, &env).await;
        }
    }

    /// Exit code 2 from a stop hook means "not done yet": its stderr goes back to the model as the next instruction.
    pub async fn trigger_stop(&self, user_message: &str, agent_response: &str) -> Option<String> {
        let mut env = self.build_env(HookTrigger::Stop, None, Some(user_message));
        env.insert("AI_AGENT_RESPONSE".into(), agent_response.into());
        for h in self.matching(HookTrigger::Stop, None) {
            let (code, err) = self.run_hook(h, &env).await;
            if code == 2 {
                return Some(format!("[stop hook '{}']: {}", h.name, if err.is_empty() { "not finished yet" } else { &err }));
            }
        }
        None
    }
}
