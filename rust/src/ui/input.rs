//! The input line: history, Tab completion for `/commands`, `/model` ids and `@files`, Ctrl+J for a newline.
use crate::util::search::list_files;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::FileHistory;
use rustyline::validate::Validator;
use rustyline::{Cmd, CompletionType, Config, Editor, EventHandler, Helper, KeyEvent};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const COMMANDS: &[(&str, &str)] = &[
    ("/help", "show commands and shortcuts"),
    ("/clear", "clear the conversation"),
    ("/compact", "summarize older messages now to free context"),
    ("/cost", "tokens, cache hit rate, cache misses, estimated cost"),
    ("/model", "show or switch model (Tab completes ids)"),
    ("/connect", "use a model API directly: /connect groq asks for the key, saves it, lists its models"),
    ("/approval", "set approval mode: on-request, auto-edit, auto, never, yolo, plan"),
    ("/agents", "list sub-agents (built-in and .claude/agents/*.md or .melon/agents/*.md)"),
    ("/permissions", "list or edit allow / deny / ask rules, e.g. /permissions deny Read(.env*)"),
    ("/effort", "how hard the model thinks: low, medium, high or auto (Codex models)"),
    ("/plan", "toggle plan mode: read-only, melon investigates and presents a plan before touching anything"),
    ("/diff", "what changed in the working tree (git diff --stat, /diff full for the patch)"),
    ("/context", "how full the context window is and what is in it"),
    ("/hooks", "list the configured hooks"),
    ("/config", "show current configuration"),
    ("/stats", "session statistics"),
    ("/tools", "list available tools"),
    ("/skills", "list project playbooks"),
    ("/verbose", "switch between quiet output (default) and full tool output"),
    ("/more", "allow 10 more tool rounds after a read-only request hit its budget"),
    ("/challenge", "a second model argues against your idea"),
    ("/mcp", "MCP server status"),
    ("/copy", "copy the last reply to the clipboard"),
    ("/jobs", "background commands: /jobs, /jobs 1 for output, /jobs kill 1"),
    ("/expand", "show the full output of the last tool call (/expand 2 for the one before)"),
    ("/save", "save this session"),
    ("/sessions", "list saved sessions"),
    ("/resume", "resume a saved session (give its ID)"),
    ("/checkpoint", "create a checkpoint"),
    ("/learn", "teach melon a rule for this repo (kept between sessions)"),
    ("/lessons", "list what melon has learned here"),
    ("/forget", "remove a lesson by number"),
    ("/rewind", "list requests that changed files; /rewind <n> puts files back to before request n"),
    ("/verify", "run the checks (tests, lint, types) on the files the last request changed"),
    ("/undo", "put back the files the last request changed"),
    ("/restore", "restore a checkpoint (give its ID)"),
    ("/exit", "quit"),
];

pub struct Completions {
    cwd: PathBuf,
    forbidden: Vec<String>,
    models: Box<dyn Fn() -> Vec<String> + Send + Sync>,
    skills: Arc<Mutex<Vec<(String, String)>>>,
    files: Mutex<(Instant, Vec<String>)>,
}

impl Completions {
    fn file_list(&self) -> Vec<String> {
        let mut f = self.files.lock().unwrap();
        if f.1.is_empty() || f.0.elapsed().as_secs() > 20 {
            f.1 = list_files(&self.cwd, &self.forbidden).iter().filter_map(|p| p.strip_prefix(&self.cwd).ok().map(|r| r.to_string_lossy().replace('\\', "/"))).collect();
            f.0 = Instant::now();
        }
        f.1.clone()
    }
}

impl Completions {
    pub fn new(cwd: PathBuf, forbidden: Vec<String>, models: Box<dyn Fn() -> Vec<String> + Send + Sync>, skills: Arc<Mutex<Vec<(String, String)>>>) -> Completions {
        Completions { cwd, forbidden, models, skills, files: Mutex::new((Instant::now(), vec![])) }
    }

    /// (start of the text being completed, candidates) for `line` with the cursor at byte `pos`.
    pub fn complete(&self, line: &str, pos: usize) -> rustyline::Result<(usize, Vec<Pair>)> {
        let text = &line[..pos];
        let pair = |display: String, replacement: String| Pair { display, replacement };
        if text.starts_with('/') && !text.contains(' ') {
            let mut out: Vec<Pair> = COMMANDS.iter().filter(|(c, _)| c.starts_with(text)).map(|(c, d)| pair(format!("{c}  {d}"), c.to_string())).collect();
            let customs = crate::util::templates::load_commands(&crate::util::templates::prompt_dirs(Some(&self.cwd)));
            out.extend(customs.iter().filter(|(n, _)| format!("/{n}").starts_with(text) && !COMMANDS.iter().any(|c| c.0 == format!("/{n}"))).map(|(n, (_, d))| pair(format!("/{n}  {d}"), format!("/{n}"))));
            out.extend(self.skills.lock().unwrap().iter().filter(|(n, _)| format!("/{n}").starts_with(text) && !COMMANDS.iter().any(|c| c.0 == format!("/{n}"))).map(|(n, d)| pair(format!("/{n}  skill: {}", d.chars().take(80).collect::<String>()), format!("/{n}"))));
            return Ok((0, out));
        }
        if let Some(arg) = text.strip_prefix("/connect ") {
            let out = crate::client::providers::PRESET_NAMES.iter().filter(|n| n.starts_with(&arg.to_lowercase())).map(|n| pair(n.to_string(), n.to_string())).collect();
            return Ok((text.len() - arg.len(), out));
        }
        if let Some(arg) = text.strip_prefix("/model ") {
            let out = (self.models)().into_iter().filter(|m| m.to_lowercase().contains(&arg.to_lowercase())).take(60).map(|m| pair(m.clone(), m)).collect();
            return Ok((text.len() - arg.len(), out));
        }
        if let Some(at) = text.rfind('@') {
            let q = &text[at + 1..];
            if !q.contains(char::is_whitespace) && (at == 0 || text[..at].ends_with(char::is_whitespace)) {
                let ql = q.to_lowercase();
                let mut hits: Vec<String> = self.file_list().into_iter().filter(|f| f.to_lowercase().contains(&ql)).collect();
                hits.sort_by_key(|f| (!f.to_lowercase().starts_with(&ql), !std::path::Path::new(f).file_name().map_or(false, |n| n.to_string_lossy().to_lowercase().contains(&ql)), f.len()));
                return Ok((at, hits.into_iter().take(40).map(|f| pair(f.clone(), format!("@{f}"))).collect()));
            }
        }
        Ok((pos, vec![]))
    }
}

pub struct MelonHelper {
    inner: Arc<Completions>,
}

impl Completer for MelonHelper {
    type Candidate = Pair;

    fn complete(&self, line: &str, pos: usize, _ctx: &rustyline::Context<'_>) -> rustyline::Result<(usize, Vec<Pair>)> {
        self.inner.complete(line, pos)
    }
}

impl Hinter for MelonHelper {
    type Hint = String;
}
impl Highlighter for MelonHelper {}
impl Validator for MelonHelper {}
impl Helper for MelonHelper {}

pub struct Input {
    editor: Arc<Mutex<Editor<MelonHelper, FileHistory>>>,
    history_file: Option<PathBuf>,
    pub skills: Arc<Mutex<Vec<(String, String)>>>,
}

pub enum Line {
    Text(String),
    /// Ctrl+C at an empty prompt.
    Interrupted,
    /// Ctrl+D.
    Eof,
}

impl Input {
    pub fn new(cwd: PathBuf, forbidden: Vec<String>, models: Box<dyn Fn() -> Vec<String> + Send + Sync>, history_file: Option<PathBuf>) -> Input {
        let cfg = Config::builder().completion_type(CompletionType::List).auto_add_history(false).history_ignore_dups(true).unwrap().build();
        let skills = Arc::new(Mutex::new(vec![]));
        let mut ed: Editor<MelonHelper, FileHistory> = Editor::with_config(cfg).expect("line editor");
        ed.set_helper(Some(MelonHelper { inner: Arc::new(Completions::new(cwd, forbidden, models, skills.clone())) }));
        ed.bind_sequence(KeyEvent::ctrl('J'), EventHandler::Simple(Cmd::Newline));
        if let Some(h) = &history_file {
            if let Some(d) = h.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = ed.load_history(h);
        }
        Input { editor: Arc::new(Mutex::new(ed)), history_file, skills }
    }

    /// Read one (possibly multi-line) message. `prompt` is already styled.
    pub async fn read(&self, prompt: String) -> Line {
        let ed = self.editor.clone();
        let res = tokio::task::spawn_blocking(move || ed.lock().unwrap().readline(&prompt)).await;
        match res {
            Ok(Ok(line)) => {
                if !line.trim().is_empty() {
                    let mut e = self.editor.lock().unwrap();
                    let _ = e.add_history_entry(line.as_str());
                    if let Some(h) = &self.history_file {
                        let _ = e.append_history(h);
                    }
                }
                Line::Text(line)
            }
            Ok(Err(ReadlineError::Interrupted)) => Line::Interrupted,
            _ => Line::Eof,
        }
    }
}
