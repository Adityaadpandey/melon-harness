//! Problems a quick checker finds in a file the agent just changed, so a broken edit is caught in the same turn instead
//! of at the end of the run. Only problems the change introduced are reported: the old and the new text are both
//! checked and anything already there before is left out. Checkers are fast and local, and a missing one is skipped:
//!   .py          compile() for syntax, plus `ruff` (undefined names, syntax) when it is installed
//!   .json .toml  parsed in-process
//!   .ts .tsx .mts .cts  the project's own tsserver (node_modules/typescript), kept running: real type errors
//!   .js .mjs .cjs `node --check` (JSX is left to the end-of-run eslint)
//!   .go          `gofmt -e`
use crate::util::re::{lit, lit_i};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

const TIMEOUT: Duration = Duration::from_secs(8);
const TS_TIMEOUT: Duration = Duration::from_secs(20); // the first check loads the whole project; a big monorepo takes seconds
const MAX_PROBLEMS: usize = 20;
const TS_SUFFIX: &[&str] = &["ts", "tsx", "mts", "cts"];

async fn run(cmd: &[&str], text: &str, cwd: &Path) -> Option<(Option<i32>, String)> {
    let mut c = Command::new(cmd[0]);
    c.args(&cmd[1..]).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = c.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let data = text.as_bytes().to_vec();
    tokio::spawn(async move {
        let _ = stdin.write_all(&data).await;
    });
    let out = tokio::time::timeout(TIMEOUT, child.wait_with_output()).await.ok()?.ok()?; // no checker, or too slow: say nothing
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Some((out.status.code(), s))
}

async fn python(path: &Path, text: &str, cwd: &Path) -> Vec<String> {
    let script = "import sys\ntry:\n compile(sys.stdin.read(), sys.argv[1], 'exec', dont_inherit=True)\nexcept SyntaxError as e:\n print(f'{e.lineno}:{e.offset or 0} SyntaxError: {e.msg}')\nexcept ValueError:\n pass\n";
    if let Some(py) = which::which("python3").ok() {
        if let Some((_, out)) = run(&[&py.to_string_lossy(), "-c", script, &path.to_string_lossy()], text, cwd).await {
            if let Some(l) = out.lines().find(|l| l.contains("SyntaxError")) {
                return vec![l.to_string()];
            }
        }
    }
    let ruff = which::which("ruff").ok().or_else(|| Some(cwd.join(".venv/bin/ruff")).filter(|p| p.exists()));
    let Some(ruff) = ruff else { return vec![] };
    let Some((_, out)) = run(&[&ruff.to_string_lossy(), "check", "--no-cache", "--quiet", "--select", "E9,F63,F7,F82", "--output-format", "concise", "--stdin-filename", &path.to_string_lossy(), "-"], text, cwd).await else { return vec![] };
    let re = lit(r"^.*?:(\d+:\d+: .+)$");
    out.lines().filter_map(|l| re.captures(l).and_then(|c| c[1].clone())).collect()
}

async fn node(path: &Path, text: &str, cwd: &Path) -> Vec<String> {
    if text.contains("</") || text.contains("/>") {
        return vec![]; // JSX: not plain JavaScript, node would call every tag an error
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let esm = ext == "mjs" || (ext == "js" && lit(r"(?m)^\s*(import|export)\b").is_match(text));
    let inp = format!("--input-type={}", if esm { "module" } else { "commonjs" });
    let Some((code, out)) = run(&["node", "--no-warnings", "--check", &inp, "-"], text, cwd).await else { return vec![] };
    if code.unwrap_or(0) == 0 {
        return vec![];
    }
    let line = lit(r"\[stdin\]:(\d+)").captures(&out).and_then(|c| c[1].clone()).unwrap_or_else(|| "?".into());
    let msg = lit(r"(?m)^(\w*Error.*)$").captures(&out).and_then(|c| c[1].clone()).unwrap_or_else(|| out.trim().lines().last().unwrap_or("").to_string());
    vec![format!("{line}:0 {msg}")]
}

/// The TypeScript language server every TS project already has, in node_modules/typescript/lib/tsserver.js. It stays
/// up for the session, so after the first check (which loads the project) each one answers in well under a second.
/// A timeout skips that edit's check but leaves the server loading for the next one; only a dead server is restarted.
struct TsServer {
    script: PathBuf,
    inner: tokio::sync::Mutex<TsInner>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
}

#[derive(Default)]
struct TsInner {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    seq: i64,
}

impl TsServer {
    async fn start(&self, inner: &mut TsInner, cwd: &Path) -> std::io::Result<()> {
        if let Some(c) = inner.child.as_mut() {
            if c.try_wait()?.is_none() {
                return Ok(());
            }
        }
        let mut child = Command::new("node")
            .arg(&self.script)
            .args(["--disableAutomaticTypingAcquisition", "--suppressDiagnosticEvents"])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        inner.stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        inner.child = Some(child);
        let pending = self.pending.clone();
        // hand each response to whoever asked for it; messages arrive as `Content-Length: N`, a blank line, N bytes of JSON
        tokio::spawn(async move {
            let mut r = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match r.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if let Some(n) = line.strip_prefix("Content-Length:").and_then(|n| n.trim().parse::<usize>().ok()) {
                    let mut blank = String::new();
                    let _ = r.read_line(&mut blank).await;
                    let mut buf = vec![0u8; n];
                    if r.read_exact(&mut buf).await.is_err() {
                        break;
                    }
                    if let Ok(msg) = serde_json::from_slice::<Value>(&buf) {
                        if msg["type"] == "response" {
                            if let Some(tx) = msg["request_seq"].as_i64().and_then(|s| pending.lock().unwrap().remove(&s)) {
                                let _ = tx.send(msg["body"].clone());
                            }
                        }
                    }
                }
            }
            pending.lock().unwrap().clear(); // dropped senders wake the waiters with an error
        });
        Ok(())
    }

    async fn send(&self, inner: &mut TsInner, command: &str, arguments: Value, reply: bool) -> std::io::Result<Option<oneshot::Receiver<Value>>> {
        inner.seq += 1;
        let rx = if reply {
            let (tx, rx) = oneshot::channel();
            self.pending.lock().unwrap().insert(inner.seq, tx);
            Some(rx)
        } else {
            None
        };
        let msg = json!({"seq": inner.seq, "type": "request", "command": command, "arguments": arguments});
        inner.stdin.as_mut().ok_or_else(|| std::io::Error::other("no stdin"))?.write_all(format!("{msg}\n").as_bytes()).await?;
        Ok(rx)
    }

    async fn errors(&self, path: &Path, text: &str, cwd: &Path) -> Vec<String> {
        let mut inner = self.inner.lock().await;
        let result = async {
            self.start(&mut inner, cwd).await?;
            let file = crate::util::paths::resolve(path).to_string_lossy().to_string();
            self.send(&mut inner, "updateOpen", json!({"openFiles": [{"file": file, "fileContent": text}]}), false).await?;
            let mut found = vec![];
            for command in ["syntacticDiagnosticsSync", "semanticDiagnosticsSync"] {
                let rx = self.send(&mut inner, command, json!({"file": file}), true).await?.unwrap();
                match tokio::time::timeout(TS_TIMEOUT, rx).await {
                    Err(_) => return Ok::<_, std::io::Error>(None), // still loading the project: the next edit gets an answer
                    Ok(Err(_)) => return Err(std::io::Error::other("tsserver exited")),
                    Ok(Ok(body)) => {
                        for d in body.as_array().into_iter().flatten() {
                            if d["category"].as_str().unwrap_or("error") == "error" {
                                found.push(format!("{}:{} TS{}: {}", d["start"]["line"], d["start"]["offset"], d["code"].as_i64().map(|c| c.to_string()).unwrap_or_default(), d["text"].as_str().unwrap_or("")));
                            }
                        }
                    }
                }
            }
            // leave tsserver on what is really on disk, or the next file's check would see this one's old text
            let on_disk = std::fs::read_to_string(path).unwrap_or_default();
            if on_disk != text {
                self.send(&mut inner, "updateOpen", json!({"openFiles": [{"file": file, "fileContent": on_disk}]}), false).await?;
            }
            Ok(Some(found))
        }
        .await;
        match result {
            Ok(Some(f)) => f,
            Ok(None) => vec![],
            Err(_) => {
                inner.child = None; // dead: restarted on the next edit
                inner.stdin = None;
                vec![]
            }
        }
    }
}

fn servers() -> &'static Mutex<HashMap<PathBuf, Arc<TsServer>>> {
    static S: OnceLock<Mutex<HashMap<PathBuf, Arc<TsServer>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The tsserver of the package the file lives in, looking up from its folder to the workspace root.
pub fn find_tsserver(path: &Path, cwd: &Path) -> Option<PathBuf> {
    let root = crate::util::paths::resolve(cwd);
    for d in crate::util::paths::resolve(path).ancestors().skip(1) {
        let script = d.join("node_modules/typescript/lib/tsserver.js");
        if script.is_file() {
            return Some(script);
        }
        if d == root || !d.starts_with(&root) {
            return None;
        }
    }
    None
}

async fn typescript(path: &Path, text: &str, cwd: &Path) -> Vec<String> {
    let Some(script) = find_tsserver(path, cwd) else { return vec![] }; // no TypeScript here: eslint at the end of the run still looks
    if which::which("node").is_err() {
        return vec![];
    }
    let server = servers().lock().unwrap().entry(script.clone()).or_insert_with(|| Arc::new(TsServer { script, inner: Default::default(), pending: Default::default() })).clone();
    server.errors(path, text, cwd).await
}

async fn go(_path: &Path, text: &str, cwd: &Path) -> Vec<String> {
    if which::which("gofmt").is_err() {
        return vec![];
    }
    let Some((code, out)) = run(&["gofmt", "-e"], text, cwd).await else { return vec![] };
    if code.unwrap_or(0) == 0 {
        return vec![];
    }
    let re = lit(r"^<standard input>:(\d+:\d+: .+)$");
    out.lines().filter_map(|l| re.captures(l).and_then(|c| c[1].clone())).collect()
}

fn data(path: &Path, text: &str) -> Vec<String> {
    if path.extension().map_or(false, |e| e == "json") {
        match serde_json::from_str::<Value>(text) {
            Err(e) => vec![format!("{}:{} {}", e.line(), e.column(), e)],
            Ok(_) => vec![],
        }
    } else {
        match text.parse::<toml::Table>() {
            Err(e) => vec![format!("0:0 {e}")],
            Ok(_) => vec![],
        }
    }
}

pub async fn problems(path: &Path, text: &str, cwd: &Path) -> Vec<String> {
    let suffix = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match suffix.as_str() {
        "py" => python(path, text, cwd).await,
        s if TS_SUFFIX.contains(&s) => typescript(path, text, cwd).await,
        "js" | "mjs" | "cjs" if which::which("node").is_ok() => node(path, text, cwd).await,
        "go" => go(path, text, cwd).await,
        "json" | "toml" => data(path, text),
        _ => vec![],
    }
}

fn key(problem: &str) -> String {
    // the same error moved down by an insert is still the same error
    lit(r"^[\d?]+:\d+:? ").replace(problem, "").to_string()
}

/// Problems in `new` that `old` did not have, line:col first, at most MAX_PROBLEMS.
pub async fn new_problems(path: &Path, old: Option<&str>, new: &str, cwd: &Path) -> Vec<String> {
    let after = problems(path, new, cwd).await;
    if after.is_empty() {
        return vec![];
    }
    let before: std::collections::HashSet<String> = match old {
        Some(o) if !o.is_empty() => problems(path, o, cwd).await.iter().map(|p| key(p)).collect(),
        _ => Default::default(),
    };
    after.into_iter().filter(|p| !before.contains(&key(p))).take(MAX_PROBLEMS).collect()
}

pub fn render(rel: &str, found: &[String]) -> String {
    let lines = found.iter().map(|p| format!("  {p}")).collect::<Vec<_>>().join("\n");
    format!("\n\n<diagnostics file=\"{rel}\">\nYour change left these problems in the file; fix them before moving on:\n{lines}\n</diagnostics>")
}

#[allow(dead_code)]
fn _silence() {
    let _ = lit_i("x");
}
