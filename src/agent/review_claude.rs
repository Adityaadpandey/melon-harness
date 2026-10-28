//! Run a review sub-agent as a headless Claude Code session (`claude -p`) instead of a melon agent. It uses the user's
//! own Claude login and quota, loads the repo's CLAUDE.md and skills the way Claude Code always does, and gets only
//! read-only tools. Nothing here talks to an undocumented API: it is the public CLI.
use crate::config::Config;
use serde_json::Value;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const GIT_READS: &[&str] = &["log", "blame", "show", "diff", "status", "ls-files", "grep", "merge-base", "rev-parse", "branch"];

pub fn available() -> bool {
    which::which("claude").is_ok()
}

pub fn build_command(config: &Config, prompt: &str, model: &str) -> Vec<String> {
    let mut cmd: Vec<String> = ["claude", "-p", prompt, "--output-format", "json", "--no-session-persistence", "--tools", "Read,Grep,Glob,Bash", "--allowedTools", "Read", "Grep", "Glob"].iter().map(|s| s.to_string()).collect();
    cmd.extend(GIT_READS.iter().map(|g| format!("Bash(git {g}:*)")));
    cmd.extend(["--disallowedTools", "Edit", "Write", "NotebookEdit"].iter().map(|s| s.to_string()));
    cmd.extend(config.forbidden_paths.iter().map(|g| format!("Read(**/{g})"))); // the repo's secret-file rules follow the review into Claude Code
    if !model.is_empty() {
        cmd.push("--model".into());
        cmd.push(model.into());
    }
    cmd
}

/// (answer text, tokens spent) from `--output-format json`; ("", 0) when it is not what we expect.
pub fn parse_output(raw: &str) -> (String, i64) {
    let Ok(mut data) = serde_json::from_str::<Value>(raw) else { return (String::new(), 0) };
    if let Value::Array(a) = &data {
        // some versions print the whole event list: the last result entry is the answer
        data = a.iter().rev().find(|d| d["type"] == "result").cloned().unwrap_or(Value::Null);
    }
    if !data.is_object() || data["is_error"].as_bool().unwrap_or(false) {
        return (String::new(), 0);
    }
    let u = &data["usage"];
    (data["result"].as_str().unwrap_or("").to_string(), u["input_tokens"].as_i64().unwrap_or(0) + u["output_tokens"].as_i64().unwrap_or(0))
}

/// One read-only Claude Code run in the repo. A missing CLI, a timeout or a bad reply is an empty answer, never an error.
pub async fn ask_claude(config: &Config, prompt: &str, timeout: u64) -> (String, i64) {
    let argv = build_command(config, prompt, &config.review_claude_model);
    // the review runs on your own Claude login, never through a gateway even if a shell or .env happens to export these
    let env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| !matches!(k.as_str(), "ANTHROPIC_BASE_URL" | "ANTHROPIC_API_KEY" | "ANTHROPIC_AUTH_TOKEN")).collect();
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(&config.cwd).env_clear().envs(env).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let Ok(child) = crate::util::detach::detach(&mut cmd).spawn() else { return (String::new(), 0) };
    match tokio::time::timeout(Duration::from_secs(timeout), child.wait_with_output()).await {
        Ok(Ok(o)) => parse_output(&String::from_utf8_lossy(&o.stdout)),
        _ => (String::new(), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses() {
        assert_eq!(parse_output(r#"{"result": "ok", "usage": {"input_tokens": 3, "output_tokens": 4}}"#), ("ok".into(), 7));
        assert_eq!(parse_output(r#"[{"type": "x"}, {"type": "result", "result": "r"}]"#).0, "r");
        assert_eq!(parse_output(r#"{"is_error": true, "result": "x"}"#).0, "");
        assert_eq!(parse_output("nope").0, "");
    }
}
