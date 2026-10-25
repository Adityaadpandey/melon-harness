use crate::config::{Config, ConfigHandle};
use crate::tools::{schema_of, Tool, ToolConfirmation, ToolInvocation, ToolKind, ToolResult};
use crate::util::fnmatch::fnmatch;
use crate::util::re::lit;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Notify;

const BLOCKED_COMMANDS: &[&str] = &[
    "rm -rf /", "rm -rf ~", "rm -rf /*", "dd if=/dev/zero", "dd if=/dev/random", "mkfs", "fdisk", "parted", ":(){ :|:& };:", "chmod 777 /", "chmod -R 777", "shutdown",
    "reboot", "halt", "poweroff", "init 0", "init 6",
];
/// Seconds a foreground command may run before it is moved to the background.
const AUTO_BACKGROUND_AFTER: u64 = 15;

/// A command running detached, e.g. `pnpm build`. Output goes to a log file the model can tail.
pub struct BackgroundJob {
    pub id: usize,
    pub command: String,
    pub log: PathBuf,
    pub pid: u32,
    pub started: Instant,
    code: Arc<Mutex<Option<i32>>>,
    announced: Mutex<bool>,
}

impl BackgroundJob {
    pub fn done(&self) -> bool {
        self.code.lock().unwrap().is_some()
    }
    pub fn returncode(&self) -> Option<i32> {
        *self.code.lock().unwrap()
    }
    pub fn tail(&self, lines: usize) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let all: Vec<&str> = text.trim_end().lines().collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }
}

fn jobs() -> &'static Mutex<HashMap<usize, Arc<BackgroundJob>>> {
    static J: OnceLock<Mutex<HashMap<usize, Arc<BackgroundJob>>>> = OnceLock::new();
    J.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Jobs that ended since the last call, each returned once.
pub fn finished_jobs() -> Vec<Arc<BackgroundJob>> {
    let map = jobs().lock().unwrap();
    let mut out: Vec<Arc<BackgroundJob>> = map.values().filter(|j| j.done() && !*j.announced.lock().unwrap()).cloned().collect();
    out.sort_by_key(|j| j.id);
    for j in &out {
        *j.announced.lock().unwrap() = true;
    }
    out
}

fn killpg(pid: u32) {
    unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
}

/// Every background job, oldest first.
pub fn jobs_list() -> Vec<Arc<BackgroundJob>> {
    let mut v: Vec<Arc<BackgroundJob>> = jobs().lock().unwrap().values().cloned().collect();
    v.sort_by_key(|j| j.id);
    v
}

pub fn kill_job(j: &BackgroundJob) {
    if !j.done() {
        killpg(j.pid);
    }
}

pub fn kill_jobs() {
    for j in jobs().lock().unwrap().values() {
        if !j.done() {
            killpg(j.pid);
        }
    }
}

const NO_MATCH: &str = "no matches found";
const DIFFER: &str = "files differ";
const FALSE: &str = "condition is false";

/// What a non-zero exit means for the program that ran last, so `grep` finding nothing is not read as a failure.
/// e.g. ("rg TODO src", 1) -> "no matches found"; ("pytest -x", 1) -> None; (anything, -9) -> "killed by signal 9".
pub fn exit_meaning(command: &str, code: Option<i32>) -> Option<String> {
    let code = code?;
    if code == 0 {
        return None;
    }
    if code < 0 || (code > 128 && code < 160) {
        // bash reports a child killed by signal N as 128+N
        let sig = if code < 0 { -code } else { code - 128 };
        return Some(format!("killed by signal {sig}{}", if sig == 9 { ", often out of memory" } else { "" }));
    }
    let last_cmd = lit(r"\|\||&&|[|;]").split_last(command);
    let last: Vec<&str> = last_cmd.split_whitespace().collect();
    let prog = last.first().map(|p| Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).unwrap_or_default();
    if code == 1 {
        if ["grep", "egrep", "fgrep", "rg", "ag", "ack", "git"].contains(&prog.as_str()) && (prog != "git" || last.get(1) == Some(&"grep")) {
            return Some(NO_MATCH.into());
        }
        if prog == "diff" || prog == "cmp" {
            return Some(DIFFER.into());
        }
        if prog == "test" || prog == "[" {
            return Some(FALSE.into());
        }
        if prog == "find" {
            return Some("some directories were not readable".into());
        }
    }
    None
}

trait SplitLast {
    fn split_last(&self, text: &str) -> String;
}
impl SplitLast for crate::util::re::Re {
    /// The text after the last separator match.
    fn split_last(&self, text: &str) -> String {
        let (mut at, mut last_end) = (0, 0);
        while at <= text.len() {
            let Some((s, e)) = self.find(&text[at..]) else { break };
            last_end = at + e;
            at += e.max(s + 1);
        }
        text[last_end..].to_string()
    }
}

/// Process env with secret-looking variables (*KEY*, *TOKEN*, ...) removed.
pub fn build_environment(config: &Config) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    let policy = &config.shell_environment;
    if !policy.ignore_default_excludes {
        for pat in &policy.exclude_patterns {
            let keys: Vec<String> = env.keys().filter(|k| fnmatch(&k.to_uppercase(), &pat.to_uppercase())).cloned().collect();
            for k in keys {
                env.remove(&k);
            }
        }
    }
    for (k, v) in &policy.set_vars {
        env.insert(k.clone(), v.clone());
    }
    env
}

pub struct ShellTool {
    config: ConfigHandle,
}

impl ShellTool {
    pub fn new(config: ConfigHandle) -> Self {
        ShellTool { config }
    }
}

struct KillOnDrop(Option<u32>);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // user interrupt: never leave the process group running
        if let Some(p) = self.0 {
            killpg(p);
        }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> String {
        "Run a shell command (builds, tests, git, scripts). Every call starts fresh in the workspace root (or `cwd`): a `cd`, variables and aliases do not carry over to the next call. Do not use it to read, search or edit files: read_file, grep, glob and edit exist for that and are cheaper to review. Quote paths with spaces. Prefer absolute paths over cd. Chain dependent steps with && and put independent ones in separate parallel calls. Do not poll with sleep; a slow command moves to the background and you are told when it ends. Never skip git hooks or force-push unless asked, and run `git status` before anything that discards changes.".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Shell
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> Value {
        schema_of(
            json!({
                "command": {"type": "string", "description": "The shell command to execute"},
                "timeout": {"type": "integer", "minimum": 1, "maximum": 600, "default": 120, "description": "Timeout in seconds (default: 120)"},
                "cwd": {"type": ["string", "null"], "description": "Working directory for the command"},
                "background": {"type": "boolean", "default": false, "description": "Detach at once and return a log path. Not required: any command still running after ~15s is moved to the background automatically, and you are told when it finishes."},
            }),
            &["command"],
        )
    }

    async fn get_confirmation(&self, inv: &ToolInvocation) -> Option<ToolConfirmation> {
        let command = inv.str("command")?.to_string();
        let blocked = BLOCKED_COMMANDS.iter().any(|b| command.contains(b));
        Some(ToolConfirmation {
            tool_name: "shell".into(),
            params: inv.params.clone(),
            description: format!("Execute{}: {command}", if blocked { " (BLOCKED)" } else { "" }),
            diff: None,
            affected_paths: vec![],
            command: Some(command),
            is_dangerous: blocked,
        })
    }

    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let cfg = self.config.get();
        let command = inv.str("command").unwrap_or("").to_string();
        let timeout = inv.params["timeout"].as_u64().unwrap_or(120);
        let background = inv.params["background"].as_bool().unwrap_or(false);
        let low = command.to_lowercase();
        let low = low.trim();
        if BLOCKED_COMMANDS.iter().any(|b| low.contains(b)) {
            return ToolResult::error_with(format!("Command blocked for safety: {command}"), json!({"blocked": true}));
        }
        let cwd = match inv.str("cwd").filter(|c| !c.is_empty()) {
            Some(c) => {
                let p = PathBuf::from(c);
                if p.is_absolute() { p } else { inv.cwd.join(p) }
            }
            None => inv.cwd.clone(),
        };
        if !cwd.exists() {
            return ToolResult::error(format!("Working directory doesn't exist: {}", cwd.display()));
        }
        let argv = crate::safety::sandbox::wrap_command(&command, vec!["/bin/bash".into(), "-c".into(), command.clone()], &cfg);
        let mut child = match Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&cwd)
            .env_clear()
            .envs(build_environment(&cfg))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("Failed to start command: {e}")),
        };
        let pid = child.id().unwrap_or(0);
        let mut guard = KillOnDrop(Some(pid));

        // output is copied to a log as it arrives, so a slow command can be handed over to the background mid-run
        let job_id = jobs().lock().unwrap().len() + 1;
        let log = std::env::temp_dir().join(format!("melon-job-{}-{job_id}.log", std::process::id()));
        let _ = std::fs::write(&log, b"");
        let bufs: Arc<Mutex<(Vec<u8>, Vec<u8>)>> = Arc::new(Mutex::new((vec![], vec![])));
        let pump = |mut stream: Box<dyn tokio::io::AsyncRead + Unpin + Send>, which: usize| {
            let bufs = bufs.clone();
            let log = log.clone();
            tokio::spawn(async move {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().append(true).open(&log).ok();
                let mut chunk = vec![0u8; 65536];
                while let Ok(n) = stream.read(&mut chunk).await {
                    if n == 0 {
                        break;
                    }
                    {
                        let mut b = bufs.lock().unwrap();
                        if which == 0 { b.0.extend_from_slice(&chunk[..n]) } else { b.1.extend_from_slice(&chunk[..n]) }
                    }
                    if let Some(f) = f.as_mut() {
                        let _ = f.write_all(&chunk[..n]);
                        let _ = f.flush();
                    }
                }
            })
        };
        let h_out = pump(Box::new(child.stdout.take().unwrap()), 0);
        let h_err = pump(Box::new(child.stderr.take().unwrap()), 1);
        let code: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let done = Arc::new(Notify::new());
        {
            let (code, done) = (code.clone(), done.clone());
            tokio::spawn(async move {
                let st = child.wait().await.ok();
                #[allow(unused_imports)]
                use std::os::unix::process::ExitStatusExt;
                let c = st.map(|s| s.code().unwrap_or_else(|| -(s.signal().unwrap_or(1)))).unwrap_or(-1);
                // grandchildren can hold the pipes open; give the pumps a moment, not forever
                let _ = tokio::time::timeout(Duration::from_secs(2), async {
                    let _ = h_out.await;
                    let _ = h_err.await;
                })
                .await;
                *code.lock().unwrap() = Some(c);
                done.notify_waiters();
                done.notify_one();
            });
        }

        // background=true detaches at once; otherwise a command still running after AUTO_BACKGROUND_AFTER seconds is detached too
        let wait_for = if background { Duration::from_millis(10) } else { Duration::from_secs(timeout.min(AUTO_BACKGROUND_AFTER)) };
        let finished = tokio::time::timeout(wait_for, async {
            while code.lock().unwrap().is_none() {
                let _ = tokio::time::timeout(Duration::from_millis(50), done.notified()).await;
            }
        })
        .await
        .is_ok();
        if !finished {
            if background || timeout > AUTO_BACKGROUND_AFTER {
                guard.0 = None;
                let job = Arc::new(BackgroundJob { id: job_id, command: command.clone(), log: log.clone(), pid, started: Instant::now(), code: code.clone(), announced: Mutex::new(false) });
                jobs().lock().unwrap().insert(job_id, job);
                return ToolResult::ok_with(
                    format!(
                        "{} background job {job_id}: {command}\nOutput so far and onward: {} (read it with `tail`). Stop it with `kill -- -{pid}`. You will be told when it finishes; keep working on other things meanwhile.",
                        if background { "Started".to_string() } else { format!("Still running after {AUTO_BACKGROUND_AFTER}s, moved to") },
                        log.display()
                    ),
                    json!({"background_job": job_id}),
                );
            }
            killpg(pid);
            return ToolResult::error(format!("Command timed out after {timeout}s"));
        }
        guard.0 = None;
        let (so, se) = {
            let b = bufs.lock().unwrap();
            (String::from_utf8_lossy(&b.0).to_string(), String::from_utf8_lossy(&b.1).to_string())
        };
        let exit_code = *code.lock().unwrap();
        let mut output = String::new();
        if !so.trim().is_empty() {
            output.push_str(so.trim_end());
        }
        if !se.trim().is_empty() {
            output.push_str("\n--- stderr ---\n");
            output.push_str(se.trim_end());
        }
        let meaning = exit_meaning(&command, exit_code);
        if exit_code != Some(0) {
            output.push_str(&format!("\nExit code: {}{}", exit_code.unwrap_or(-1), meaning.as_ref().map(|m| format!(" ({m})")).unwrap_or_default()));
        } else if output.is_empty() {
            output = "(command succeeded with no output)".into();
        }
        let ok = exit_code == Some(0) || meaning.as_deref().map_or(false, |m| m == NO_MATCH || m == DIFFER || m == FALSE);
        // the registry applies the model-facing cap; this only bounds memory
        if output.len() > 400 * 1024 {
            output = format!("{}\n... [output truncated]", crate::util::text::clip(&output, 400 * 1024));
        }
        ToolResult { success: ok, output, error: if ok { None } else { Some(se) }, exit_code, ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn meanings() {
        assert_eq!(exit_meaning("rg TODO src", Some(1)).as_deref(), Some(NO_MATCH));
        assert_eq!(exit_meaning("ls && grep x f", Some(1)).as_deref(), Some(NO_MATCH));
        assert_eq!(exit_meaning("pytest -x", Some(1)), None);
        assert_eq!(exit_meaning("x", Some(137)).as_deref(), Some("killed by signal 9, often out of memory"));
        assert_eq!(exit_meaning("diff a b", Some(1)).as_deref(), Some(DIFFER));
        assert_eq!(exit_meaning("git status", Some(1)), None);
    }
}
