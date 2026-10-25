//! The live view of a turn: assistant text as rail-marked blocks, one line (or a framed block) per tool call, a
//! shimmering status line while the agent works, sub-agent rows and the task list.
use super::md::{render_block, split_blocks};
use super::style::*;
use crate::tools::ToolConfirmation;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const SPIN: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const QUIET_COMMANDS: &[&str] = &["grep", "rg", "find", "ls", "cat", "head", "tail", "wc", "tree", "pwd", "echo", "fd", "which"];
const ROUTINE_NOTICES: &[&str] = &["Loaded playbooks", "Scout finished", "Routing to", "Working in", "Challenger is questioning", "steering received", "Context compacted", "Cache had expired"];
pub const TIPS: &[&str] = &[
    "@path attaches a file · Tab completes",
    "!git status runs a command and shows it to the model",
    "/compact frees context when a long session slows down",
    "/cost shows tokens, cache hits and why a cache missed",
    "Ctrl+J inserts a newline",
    "Several read-only sub-agents in one turn run in parallel",
];

fn title_of(name: &str) -> &str {
    match name {
        "read_file" => "Read",
        "write_file" => "Write",
        "edit" => "Update",
        "shell" => "Bash",
        "grep" => "Search",
        "glob" => "Find",
        "list_dir" => "List",
        "web_search" => "Web Search",
        "web_fetch" => "Fetch",
        "todos" => "Todos",
        "ask_user" => "Question",
        "memory" => "Memory",
        "use_skill" => "Skill",
        other => other,
    }
}

fn primary_arg(name: &str) -> Option<&'static str> {
    Some(match name {
        "read_file" | "write_file" | "edit" | "list_dir" => "path",
        "shell" => "command",
        "grep" | "glob" => "pattern",
        "web_search" => "query",
        "web_fetch" => "url",
        "use_skill" => "name",
        _ => return None,
    })
}

fn kind_color(name: &str) -> &'static str {
    if name.starts_with("subagent_") || name == "parallel_tasks" {
        return ACCENT;
    }
    match name {
        "read_file" | "glob" | "grep" | "list_dir" | "search" | "graph" => SKY,
        "edit" | "write_file" => AMBER,
        "shell" | "ask_user" => LAVENDER,
        "web_search" | "web_fetch" => TEAL,
        "memory" | "todos" | "use_skill" => MINT,
        _ => "#a39c8c",
    }
}

pub fn agent_title(agent: &str) -> String {
    match agent {
        "investigator" | "codebase_investigator" => "Scout".into(),
        "code_reviewer" => "Reviewer".into(),
        "planner" => "Planner".into(),
        other => {
            let s = other.replace('_', " ");
            let mut c = s.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    }
}

/// (title, argument) for a call, e.g. ("Read", "src/a.py").
pub fn tool_parts(name: &str, args: &Map<String, Value>, cwd: &Path) -> (String, String) {
    if let Some(a) = name.strip_prefix("subagent_") {
        let goal = args.get("goal").and_then(Value::as_str).unwrap_or("");
        return (format!("{} agent", agent_title(a)), if goal.chars().count() > 70 { format!("{}…", goal.chars().take(70).collect::<String>()) } else { goal.to_string() });
    }
    if name == "ask_user" {
        let qs = args.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
        let first = qs.first().and_then(|q| q["question"].as_str()).unwrap_or("").to_string();
        return ("Question".into(), format!("{first}{}", if qs.len() > 1 { format!(" (+{} more)", qs.len() - 1) } else { String::new() }));
    }
    let key = primary_arg(name);
    let val = key.and_then(|k| args.get(k)).or_else(|| args.values().next());
    let mut v = match val {
        Some(Value::String(s)) => s.clone(),
        Some(o) => o.to_string(),
        None => String::new(),
    };
    if key == Some("path") {
        v = crate::util::paths::rel_display(Path::new(&v), cwd);
    }
    let v = v.replace('\n', " ");
    let v = clip_chars(&v, 90);
    let extra = if name == "grep" { args.get("glob").and_then(Value::as_str).map(|g| format!(", {g}")).unwrap_or_default() } else { String::new() };
    (title_of(name).to_string(), format!("{v}{extra}"))
}

pub fn tool_label(name: &str, args: &Map<String, Value>, cwd: &Path) -> String {
    let (t, a) = tool_parts(name, args, cwd);
    format!("{t}({a})")
}

/// `read  src/app.py`: a coloured chip for the tool, then its argument with the file name picked out of the path.
pub fn header_text(name: &str, args: &Map<String, Value>, cwd: &Path) -> String {
    let (title, arg) = tool_parts(name, args, cwd);
    let mut out = format!(" {}  ", chip(&format!(" {} ", title.to_lowercase()), kind_color(name)));
    if primary_arg(name) == Some("path") && arg.contains('/') {
        let (head, tail) = arg.rsplit_once('/').unwrap();
        out.push_str(&paint(&format!("{head}/"), MUTED));
        out.push_str(&bold_paint(tail, FG));
    } else if name == "shell" {
        out.push_str(&paint("$ ", MUTED));
        out.push_str(&bold_paint(&arg, LAVENDER));
    } else {
        out.push_str(&bold_paint(&arg, FG));
    }
    out
}

fn first_lines(text: &str, n: usize) -> (Vec<String>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    (lines.iter().take(n).map(|s| s.to_string()).collect(), lines.len().saturating_sub(n))
}

/// (kind, line_no, text) rows of a unified diff, plus add/remove counts.
pub fn parse_unified_diff(diff: &str) -> (Vec<(char, Option<usize>, String)>, usize, usize) {
    let (mut rows, mut adds, mut dels, mut old_n, mut new_n) = (vec![], 0, 0, 0usize, 0usize);
    let hunk = crate::util::re::lit(r"^@@ -(\d+)(?:,\d+)? \+(\d+)");
    for line in diff.lines() {
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        if line.starts_with("@@") {
            if let Some(c) = hunk.captures(line) {
                old_n = c[1].as_ref().and_then(|x| x.parse().ok()).unwrap_or(0);
                new_n = c[2].as_ref().and_then(|x| x.parse().ok()).unwrap_or(0);
            }
        } else if let Some(t) = line.strip_prefix('+') {
            adds += 1;
            rows.push(('+', Some(new_n), t.to_string()));
            new_n += 1;
        } else if let Some(t) = line.strip_prefix('-') {
            dels += 1;
            rows.push(('-', Some(old_n), t.to_string()));
            old_n += 1;
        } else {
            rows.push((' ', Some(new_n), line.strip_prefix(' ').unwrap_or(line).to_string()));
            old_n += 1;
            new_n += 1;
        }
    }
    (rows, adds, dels)
}

/// Rows with line numbers, a +/- gutter and a tinted background that runs the full width, so added and removed lines
/// read as blocks, not as coloured text.
pub fn render_diff(diff: &str, max_lines: usize, width: usize) -> (Vec<String>, usize, usize) {
    let (rows, adds, dels) = parse_unified_diff(diff);
    let (mut out, mut shown, mut shown_changed) = (vec![], 0, 0);
    for (kind, no, txt) in rows {
        if shown >= max_lines {
            break;
        }
        let code: String = txt.chars().take(160).collect();
        let body = format!("{:>4} {kind} {code}", no.map(|n| n.to_string()).unwrap_or_default());
        let pad = width.saturating_sub(super::style::width(&body));
        let line = match kind {
            '+' => format!("{}{}{}{}{}", bg_code(ADD_BG), fg_code(MINT), body, " ".repeat(pad), RESET),
            '-' => format!("{}{}{}{}{}", bg_code(DEL_BG), fg_code(CORAL), body, " ".repeat(pad), RESET),
            _ => paint(&body, MUTED),
        };
        out.push(if color_on() { line } else { body });
        shown += 1;
        shown_changed += usize::from(kind != ' ');
    }
    let hidden = (adds + dels).saturating_sub(shown_changed);
    if hidden > 0 {
        out.push(paint(&format!("   ⋯ +{hidden} more changed lines"), MUTED));
    }
    (out, adds, dels)
}

/// (one-line summary, preview lines) for a finished call.
pub fn summarize_result(name: &str, success: bool, output: &str, error: Option<&str>, md: &Map<String, Value>, exit_code: Option<i32>, args: &Map<String, Value>) -> (String, Vec<String>) {
    let more = |lines: &mut Vec<String>, n: usize| {
        if n > 0 {
            lines.push(format!("… +{n} lines"));
        }
    };
    if !success {
        let (lines, _) = first_lines(error.filter(|e| !e.is_empty()).unwrap_or(if output.is_empty() { "failed" } else { output }).trim(), 3);
        return (format!("✗ {}", lines.first().cloned().unwrap_or_else(|| "failed".into())), lines.into_iter().skip(1).collect());
    }
    let int = |k: &str| md.get(k).and_then(Value::as_i64);
    match name {
        "read_file" => {
            if md.get("deduped").and_then(Value::as_bool).unwrap_or(false) {
                return ("Already read (unchanged)".into(), vec![]);
            }
            let (total, a, b) = (int("total_lines"), int("shown_start"), int("shown_end"));
            let n = match (a, b) {
                (Some(a), Some(b)) => Some(b - a + 1),
                _ => total.or(Some(output.lines().count() as i64)),
            };
            let tail = match (total, n) {
                (Some(t), Some(n)) if t != n => format!(" (of {t})"),
                _ => String::new(),
            };
            (format!("Read {} lines{tail}", n.map_or("?".into(), |n| n.to_string())), vec![])
        }
        "edit" | "write_file" => (output.trim().lines().next().unwrap_or("Done").to_string(), vec![]),
        "shell" => {
            let (mut lines, m) = if output.trim().is_empty() { (vec![], 0) } else { first_lines(output.trim(), 6) };
            more(&mut lines, m);
            (if exit_code.map_or(false, |c| c != 0) { format!("exit {}", exit_code.unwrap()) } else { "Ran".into() }, lines)
        }
        "grep" => {
            let (m, f) = (int("matches").unwrap_or(0), int("files_with_matches").unwrap_or(0));
            let head = if m > 0 { format!("Found {m} match{} in {f} file{}", if m != 1 { "es" } else { "" }, if f != 1 { "s" } else { "" }) } else { "No matches".into() };
            let (mut lines, n) = if m > 0 { first_lines(output, 4) } else { (vec![], 0) };
            more(&mut lines, n);
            (head, lines)
        }
        "glob" => {
            let m = int("matches").unwrap_or(0);
            let (mut lines, n) = if m > 0 { first_lines(output, 4) } else { (vec![], 0) };
            more(&mut lines, n);
            (if m > 0 { format!("Found {m} file{}", if m != 1 { "s" } else { "" }) } else { "No files".into() }, lines)
        }
        "list_dir" => (format!("Listed {} entries", int("entries").unwrap_or(0)), vec![]),
        "use_skill" => (format!("Loaded {}{}", args.get("name").and_then(Value::as_str).unwrap_or("skill"), args.get("section").and_then(Value::as_str).map(|s| format!(" · {s}")).unwrap_or_default()), vec![]),
        n if n.starts_with("subagent_") => {
            let (mut lines, k) = first_lines(output.rsplit("Result:").next().unwrap_or("").trim(), 4);
            more(&mut lines, k);
            ("Done".into(), lines)
        }
        _ => {
            let (lines, k) = first_lines(output.trim(), 3);
            let mut rest: Vec<String> = lines.iter().skip(1).cloned().collect();
            more(&mut rest, k);
            (lines.first().cloned().unwrap_or_else(|| "Done".into()), rest)
        }
    }
}

/// A copy of what the side panel lists.
pub struct SideData {
    pub read: Vec<(String, usize)>,
    pub written: Vec<(String, (usize, usize, bool))>,
    pub commands: Vec<(String, bool)>,
    /// Keys ("read:path", "write:path", "cmd:3") touched in the last two seconds.
    pub fresh: std::collections::HashSet<String>,
    pub running: Vec<(String, String, String)>,
    pub finished: Vec<(String, bool, String)>,
}

struct AgentRow {
    agent: String,
    goal: String,
    started: Instant,
    recent: VecDeque<String>,
    tools: usize,
    tokens: i64,
}

#[derive(Clone)]
struct DoneAgent {
    agent: String,
    goal: String,
    ok: bool,
    seconds: f64,
    tools: usize,
    tokens: i64,
}

fn agent_stats(tools: usize, tokens: i64, seconds: f64) -> String {
    let mut parts = vec![format!("{}s", seconds as i64), format!("{tools} tool{}", if tools != 1 { "s" } else { "" })];
    if tokens > 0 {
        parts.push(if tokens >= 1000 { format!("{}k tok", tokens / 1000) } else { format!("{tokens} tok") });
    }
    parts.join(" · ")
}

struct State {
    calls: HashMap<String, (String, Map<String, Value>)>,
    /// An approval prompt already printed the call's header.
    header_done: bool,
    expand: bool,
    quiet: bool,
    turn_active: bool,
    turn_started: Instant,
    buf: String,
    first_block: bool,
    out_chars: usize,
    phase: String,
    todos: Vec<(String, bool)>,
    agents: HashMap<String, AgentRow>,
    agent_order: Vec<String>,
    agent_log: Vec<DoneAgent>,
    results: VecDeque<Value>,
    last_reply: String,
    paused: bool,
    status_rows: usize,
    /// The side panel is on screen and already lists every file read.
    side_panel: bool,
    /// What the side panel lists: files read (with counts), files written (+, -, new), commands run (ok).
    read_files: Vec<(String, usize)>,
    written: Vec<(String, (usize, usize, bool))>,
    commands: Vec<(String, bool)>,
    /// "kind:name" -> when it last changed, so the panel can glow.
    touched: HashMap<String, Instant>,
}

pub struct Tui {
    st: Mutex<State>,
    cwd: Mutex<PathBuf>,
    stop: Arc<AtomicBool>,
}

fn out(s: &str) {
    super::sink::write(s);
}

impl Tui {
    pub fn new(cwd: &Path) -> Arc<Tui> {
        let t = Arc::new(Tui {
            st: Mutex::new(State {
                calls: HashMap::new(),
                header_done: false,
                expand: false,
                quiet: true,
                turn_active: false,
                turn_started: Instant::now(),
                buf: String::new(),
                first_block: true,
                out_chars: 0,
                phase: "Thinking".into(),
                todos: vec![],
                agents: HashMap::new(),
                agent_order: vec![],
                agent_log: vec![],
                results: VecDeque::new(),
                last_reply: String::new(),
                paused: false,
                status_rows: 0,
                side_panel: false,
                read_files: vec![],
                written: vec![],
                commands: vec![],
                touched: HashMap::new(),
            }),
            cwd: Mutex::new(cwd.to_path_buf()),
            stop: Arc::new(AtomicBool::new(false)),
        });
        if std::io::stdout().is_terminal() && !super::sink::active() {
            // the spinner redraws the status block ten times a second while a turn is active
            let weak = Arc::downgrade(&t);
            let stop = t.stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    match weak.upgrade() {
                        Some(t) => t.redraw(),
                        None => break,
                    }
                }
            });
        }
        t
    }

    fn cwd(&self) -> PathBuf {
        self.cwd.lock().unwrap().clone()
    }

    pub fn set_cwd(&self, p: &Path) {
        *self.cwd.lock().unwrap() = p.to_path_buf();
    }

    pub fn quiet(&self) -> bool {
        self.st.lock().unwrap().quiet
    }

    pub fn toggle_quiet(&self) -> bool {
        let mut s = self.st.lock().unwrap();
        s.quiet = !s.quiet;
        s.quiet
    }

    pub fn last_reply(&self) -> String {
        self.st.lock().unwrap().last_reply.clone()
    }

    pub fn reset(&self) {
        let mut s = self.st.lock().unwrap();
        s.todos.clear();
        s.last_reply.clear();
        s.agents.clear();
        s.agent_order.clear();
        s.agent_log.clear();
        s.results.clear();
        s.buf.clear();
        s.first_block = true;
        s.read_files.clear();
        s.written.clear();
        s.commands.clear();
        s.touched.clear();
    }

    // -- the status block ---------------------------------------------------------------------------------------
    fn status_text(s: &State) -> Vec<String> {
        let now = s.turn_started.elapsed().as_secs_f64();
        let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        let frame = SPIN[((wall * 10.0) as usize) % SPIN.len()];
        let tokens = if s.out_chars > 400 { format!("  ·  ↓ ~{:.1}k tokens", s.out_chars as f64 / 4000.0) } else { String::new() };
        let mut rows = vec![format!(" {} {}{}", bold_paint(&frame.to_string(), ACCENT), shimmer(&format!("{}…", s.phase), wall), paint(&format!("   {}s{tokens}  ·  ctrl+c to stop", now as i64), MUTED))];
        for a in s.agent_log.iter() {
            let (mark, color) = if a.ok { ("✓", MINT) } else { ("✗", CORAL) };
            rows.push(format!("   {} {}{}{}", paint(mark, color), bold_paint(&agent_title(&a.agent), FG), paint(&format!("  {}", a.goal), MUTED), paint(&format!("   {}", agent_stats(a.tools, a.tokens, a.seconds)), FAINT)));
        }
        for id in &s.agent_order {
            let Some(a) = s.agents.get(id) else { continue };
            rows.push(format!("   {} {}{}{}", paint("◆", ACCENT), bold_paint(&agent_title(&a.agent), ACCENT), paint(&format!("  {}", a.goal), FG), paint(&format!("   {}", agent_stats(a.tools, a.tokens, a.started.elapsed().as_secs_f64())), MUTED)));
            let n = a.recent.len();
            for (i, label) in a.recent.iter().enumerate() {
                let last = i + 1 == n;
                rows.push(format!("   {} {} {}", paint(if last { "╰" } else { "│" }, RULE), paint(if last { "▸" } else { "✓" }, if last { ACCENT } else { FAINT }), paint(label, if last { FG } else { MUTED })));
            }
            if n == 0 {
                rows.push(format!("   {} {}", paint("╰", RULE), paint("starting…", MUTED)));
            }
        }
        if !s.todos.is_empty() {
            let done = s.todos.iter().filter(|t| t.1).count();
            rows.push(format!("{}{}", bold_paint("   ✦ tasks ", LAVENDER), paint(&format!("{done}/{}", s.todos.len()), MUTED)));
            for (c, d) in s.todos.iter().take(8) {
                rows.push(format!("     {} {}", if *d { "◼" } else { "◻" }, paint(&clip_chars(c, 90), if *d { MUTED } else { FG })));
            }
        }
        rows
    }

    fn clear_status(s: &mut State) {
        if s.status_rows > 0 {
            let mut seq = String::from("\r\x1b[2K");
            for _ in 1..s.status_rows {
                seq.push_str("\x1b[1A\x1b[2K");
            }
            out(&seq);
            s.status_rows = 0;
        }
    }

    fn draw_status(s: &mut State) {
        if !s.turn_active || s.paused || super::sink::active() || !std::io::stdout().is_terminal() {
            return;
        }
        let rows = Self::status_text(s);
        out(&rows.join("\n"));
        s.status_rows = rows.len();
    }

    fn redraw(&self) {
        let mut s = self.st.lock().unwrap();
        if s.turn_active && !s.paused {
            Self::clear_status(&mut s);
            Self::draw_status(&mut s);
        }
    }

    /// Print above the status block and put the block back under it.
    fn print_above(&self, text: &str) {
        let mut s = self.st.lock().unwrap();
        Self::clear_status(&mut s);
        out(&format!("{text}\n"));
        Self::draw_status(&mut s);
    }

    pub fn println(&self, text: &str) {
        self.print_above(text);
    }

    pub fn start_turn(&self, phase: &str) {
        let mut s = self.st.lock().unwrap();
        s.turn_active = true;
        s.turn_started = Instant::now();
        s.out_chars = 0;
        s.buf.clear();
        s.first_block = true;
        s.phase = phase.into();
    }

    pub fn end_turn(&self) {
        self.assistant_flush();
        let mut s = self.st.lock().unwrap();
        Self::clear_status(&mut s);
        s.turn_active = false;
        s.agents.clear();
        s.agent_order.clear();
        s.agent_log.clear();
    }

    pub fn is_turn_active(&self) -> bool {
        self.st.lock().unwrap().turn_active
    }

    /// Stop the live block while something else needs the terminal (an approval prompt).
    pub fn pause(&self) {
        let mut s = self.st.lock().unwrap();
        Self::clear_status(&mut s);
        s.paused = true;
    }

    pub fn resume(&self) {
        self.st.lock().unwrap().paused = false;
    }

    pub fn set_phase(&self, p: &str) {
        self.st.lock().unwrap().phase = p.into();
    }

    // -- transcript -----------------------------------------------------------------------------------------------
    pub fn print_welcome(&self, title: &str, lines: &[String]) {
        let mut text = format!("\n  {}{}\n", bold_paint(title, ACCENT), paint("  ·  a small agent for real repos", MUTED));
        for l in lines {
            text.push_str(&format!("  {}\n", paint(l, MUTED)));
        }
        out(&text);
    }

    pub fn echo_user(&self, text: &str) {
        let mut parts = text.trim().split('\n');
        let first = parts.next().unwrap_or("");
        let rest = parts.count();
        let shown = format!("{}{}", clip_chars(first, 300), if rest > 0 { format!("  (+{rest} lines)") } else { String::new() });
        let line = format!(" {}{}", bold_paint("▍ ", ACCENT), bold_paint(&shown, FG));
        self.print_above(&if color_on() { on_bg(&format!("{line} "), BG_BOX) } else { line });
    }

    fn print_block(&self, md: &str) {
        if md.trim().is_empty() {
            return;
        }
        let first = {
            let mut s = self.st.lock().unwrap();
            let f = s.first_block;
            s.last_reply = if f { md.trim().to_string() } else { format!("{}\n\n{}", s.last_reply, md.trim()) }; // for /copy
            s.first_block = false;
            f
        };
        self.print_above(&format!("\n{}", render_block(md, first)));
    }

    pub fn assistant_delta(&self, content: &str) {
        let done = {
            let mut s = self.st.lock().unwrap();
            s.buf.push_str(content);
            s.out_chars += content.len();
            s.phase = "Writing".into();
            let (done, rest) = split_blocks(&s.buf);
            if !done.is_empty() {
                s.buf = rest;
            }
            done
        };
        if !done.is_empty() {
            self.print_block(&done);
        }
    }

    pub fn assistant_flush(&self) {
        let buf = std::mem::take(&mut self.st.lock().unwrap().buf);
        if !buf.trim().is_empty() {
            self.print_block(&buf);
        }
        self.st.lock().unwrap().first_block = true; // the next piece of text (after a tool call) starts a new message
    }

    pub fn notice(&self, message: &str) {
        if self.quiet() && ROUTINE_NOTICES.iter().any(|p| message.starts_with(p)) {
            return; // things the harness did that you do not need to act on
        }
        self.print_above(&italic(&paint(&format!("  ✦ {message}"), MUTED)));
    }

    pub fn error(&self, message: &str) {
        self.assistant_flush();
        self.print_above(&format!("\n {} {}", bold_paint("✗", CORAL), paint(message, CORAL)));
    }

    pub fn set_todos(&self, todos: Vec<(String, bool)>) {
        self.st.lock().unwrap().todos = todos;
    }

    pub fn agent_progress(&self, call_id: &str, info: &Value) {
        let mut s = self.st.lock().unwrap();
        let cwd = self.cwd();
        match info["kind"].as_str().unwrap_or("") {
            "start" => {
                let goal = crate::util::text::squash(info["goal"].as_str().unwrap_or(""));
                s.agents.insert(call_id.into(), AgentRow { agent: info["agent"].as_str().unwrap_or("").into(), goal: clip_chars(&goal, 60), started: Instant::now(), recent: VecDeque::new(), tools: 0, tokens: 0 });
                s.agent_order.push(call_id.into());
            }
            "tool" => {
                if let Some(a) = s.agents.get_mut(call_id) {
                    let args = info["args"].as_object().cloned().unwrap_or_default();
                    a.recent.push_back(tool_label(info["name"].as_str().unwrap_or(""), &args, &cwd));
                    if a.recent.len() > 3 {
                        a.recent.pop_front();
                    }
                    a.tools += 1;
                    a.tokens = info["tokens"].as_i64().unwrap_or(a.tokens);
                }
                if info["name"] == "read_file" {
                    if let Some(p) = info["args"]["path"].as_str() {
                        // files a sub-agent opens count as read too
                        let path = crate::util::paths::rel_display(Path::new(p), &cwd);
                        Self::note_read(&mut s, path);
                    }
                }
            }
            "done" => {
                if let Some(mut a) = s.agents.remove(call_id) {
                    s.agent_order.retain(|x| x != call_id);
                    a.tokens = info["tokens"].as_i64().unwrap_or(a.tokens);
                    s.agent_log.push(DoneAgent { agent: a.agent, goal: a.goal, ok: info["ok"].as_bool().unwrap_or(true), seconds: a.started.elapsed().as_secs_f64(), tools: a.tools, tokens: a.tokens });
                }
            }
            _ => {}
        }
    }

    /// Only remembers the call. The header is printed together with its result (or with the approval prompt), so
    /// parallel calls never interleave.
    pub fn tool_call_start(&self, call_id: &str, name: &str, arguments: &Map<String, Value>) {
        self.assistant_flush();
        let cwd = self.cwd();
        let mut s = self.st.lock().unwrap();
        s.calls.insert(call_id.into(), (name.into(), arguments.clone()));
        s.phase = if s.calls.len() == 1 { format!("Running {}", tool_label(name, arguments, &cwd)) } else { format!("Running {} tools", s.calls.len()) };
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tool_call_complete(&self, call_id: &str, name: &str, success: bool, output: &str, error: Option<&str>, metadata: &Map<String, Value>, diff: Option<&str>, exit_code: Option<i32>) {
        let cwd = self.cwd();
        let (args, quiet, expand, header_done) = {
            let mut s = self.st.lock().unwrap();
            let args = s.calls.remove(call_id).map(|c| c.1).unwrap_or_default();
            s.phase = "Thinking".into();
            s.results.push_back(json!({"label": tool_label(name, &args, &cwd), "success": success, "output": output, "error": error, "diff": diff}));
            if s.results.len() > 30 {
                s.results.pop_front();
            }
            (args, s.quiet, s.expand, s.header_done)
        };
        self.track(name, &args, success, metadata, diff);
        let (summary, preview) = summarize_result(name, success, output, error, metadata, exit_code, &args);
        if quiet && success && name == "read_file" && self.st.lock().unwrap().side_panel {
            return; // the panel's READ list shows it
        }
        let is_edit = matches!(name, "edit" | "write_file") && success && diff.is_some();
        let plain_summary = summary.trim_start_matches("✗ ").to_string();
        if quiet && !is_edit {
            // `read  src/app.py  ·  Read 40 lines` on a single line; /expand shows the full output
            let line = if header_done {
                self.st.lock().unwrap().header_done = false; // an approval prompt already showed the header
                paint(&format!("  {} {plain_summary}", if success { "✓" } else { "✗" }), if success { MUTED } else { CORAL })
            } else {
                format!("{}{}", header_text(name, &args, &cwd), paint(&format!("  ·  {plain_summary}"), if success { MUTED } else { CORAL }))
            };
            self.print_above(&line);
            return;
        }
        if header_done {
            self.st.lock().unwrap().header_done = false;
        } else {
            self.print_above(&format!("\n{}", header_text(name, &args, &cwd)));
        }
        let color = if success { kind_color(name) } else { CORAL };
        let mut rows: Vec<String> = vec![];
        if is_edit {
            let w = term_width().min(120).saturating_sub(6);
            let (body, adds, dels) = render_diff(diff.unwrap(), if expand { 400 } else if quiet { 8 } else { 14 }, w);
            let counts = [(adds, "+", MINT), (dels, "−", CORAL)].iter().filter(|c| c.0 > 0).map(|c| bold_paint(&format!("{}{}", c.1, c.0), c.2)).collect::<Vec<_>>().join(" ");
            let verb = if metadata.get("is_new_file").and_then(Value::as_bool).unwrap_or(false) { "created" } else { "updated" };
            rows.push(format!("{}{}{counts}", bold_paint("✓ ", MINT), paint(&format!("{verb}  "), MUTED)));
            rows.extend(body);
        } else {
            rows.push(format!("{}{}", if success { bold_paint("✓ ", MINT) } else { bold_paint("✗ ", CORAL) }, paint(&plain_summary, if success { FG } else { CORAL })));
            for l in preview.iter().take(if expand { 60 } else { usize::MAX }) {
                rows.push(paint(&format!("  {}", clip_chars(l, 200)), MUTED));
            }
        }
        self.print_above(&rows.iter().map(|r| format!(" {} {r}", paint("│", color))).collect::<Vec<_>>().join("\n"));
    }

    fn note_read(s: &mut State, path: String) {
        match s.read_files.iter_mut().find(|r| r.0 == path) {
            Some(r) => r.1 += 1,
            None => s.read_files.push((path.clone(), 1)),
        }
        s.touched.insert(format!("read:{path}"), Instant::now());
    }

    /// Feeds the side panel: files read, files written, commands run.
    fn track(&self, name: &str, args: &Map<String, Value>, success: bool, md: &Map<String, Value>, diff: Option<&str>) {
        let cwd = self.cwd();
        let mut s = self.st.lock().unwrap();
        let cmd = args.get("command").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let loud = |c: &str| c.split_whitespace().next().map_or(false, |w| !QUIET_COMMANDS.contains(&w));
        if !success {
            if name == "shell" && !cmd.is_empty() && loud(&cmd) {
                s.commands.push((cmd, false));
                let k = format!("cmd:{}", s.commands.len() - 1);
                s.touched.insert(k, Instant::now());
            }
            return;
        }
        let raw = md.get("path").and_then(Value::as_str).or_else(|| args.get("path").and_then(Value::as_str)).unwrap_or("");
        let path = if raw.is_empty() { String::new() } else { crate::util::paths::rel_display(Path::new(raw), &cwd) };
        if name == "read_file" && !path.is_empty() {
            Self::note_read(&mut s, path);
        } else if matches!(name, "edit" | "write_file") && !path.is_empty() {
            let (_, adds, dels) = diff.map(parse_unified_diff).unwrap_or((vec![], 0, 0));
            let new = md.get("is_new_file").and_then(Value::as_bool).unwrap_or(false);
            match s.written.iter_mut().find(|w| w.0 == path) {
                Some(w) => w.1 = (w.1 .0 + adds, w.1 .1 + dels, w.1 .2 || new),
                None => s.written.push((path.clone(), (adds, dels, new))),
            }
            s.touched.insert(format!("write:{path}"), Instant::now());
        } else if name == "shell" && !cmd.is_empty() && loud(&cmd) {
            s.commands.push((cmd, true));
            let k = format!("cmd:{}", s.commands.len() - 1);
            s.touched.insert(k, Instant::now());
        }
    }

    pub fn set_side_panel(&self, on: bool) {
        self.st.lock().unwrap().side_panel = on;
    }

    /// Does the side panel have anything to show yet?
    pub fn side_has(&self) -> [bool; 4] {
        let s = self.st.lock().unwrap();
        [!(s.agents.is_empty() && s.agent_log.is_empty()), !s.read_files.is_empty(), !s.written.is_empty(), !s.commands.is_empty()]
    }

    pub fn snapshot_side(&self) -> SideData {
        let s = self.st.lock().unwrap();
        let now = Instant::now();
        SideData {
            read: s.read_files.clone(),
            written: s.written.clone(),
            commands: s.commands.clone(),
            fresh: s.touched.iter().filter(|(_, t)| now.duration_since(**t).as_secs_f64() < 2.0).map(|(k, _)| k.clone()).collect(),
            running: s.agent_order.iter().filter_map(|id| s.agents.get(id)).map(|a| (a.agent.clone(), a.recent.back().cloned().unwrap_or_else(|| "starting…".into()), agent_stats(a.tools, a.tokens, a.started.elapsed().as_secs_f64()))).collect(),
            finished: s.agent_log.iter().map(|a| (a.agent.clone(), a.ok, agent_stats(a.tools, a.tokens, a.seconds))).collect(),
        }
    }

    /// The working indicator as ANSI rows (the screen draws these under the transcript).
    pub fn status_lines(&self) -> Vec<String> {
        let s = self.st.lock().unwrap();
        if s.turn_active { Self::status_text(&s) } else { vec![] }
    }

    /// /expand [n]: print the full output of the n-th most recent tool call.
    pub fn show_result(&self, n: usize) {
        let r = {
            let s = self.st.lock().unwrap();
            if n < 1 || n > s.results.len() { None } else { Some(s.results[s.results.len() - n].clone()) }
        };
        let Some(r) = r else { return self.println(&paint("  no such tool result", MUTED)) };
        let body = if let Some(d) = r["diff"].as_str() {
            render_diff(d, 1000, term_width().min(120).saturating_sub(6)).0.join("\n")
        } else {
            let text = format!("{}{}{}", r["error"].as_str().unwrap_or(""), if r["error"].is_string() && !r["output"].as_str().unwrap_or("").is_empty() { "\n" } else { "" }, r["output"].as_str().unwrap_or(""));
            let lines: Vec<&str> = text.lines().collect();
            format!("{}{}", lines.iter().take(300).cloned().collect::<Vec<_>>().join("\n"), if lines.len() > 300 { format!("\n… +{} lines", lines.len() - 300) } else { String::new() })
        };
        self.println(&format!("{}\n{body}", bold_paint(&format!("╭ {}", r["label"].as_str().unwrap_or("")), FG)));
    }

    pub fn turn_summary(&self, elapsed: f64, usage: &Value) {
        let took = if elapsed >= 60.0 { format!("{}m {}s", elapsed as i64 / 60, elapsed as i64 % 60) } else { format!("{elapsed:.0}s") };
        let mut line = format!("  {}{}{}", paint("✦ ", ACCENT), paint("Worked for ", MUTED), bold_paint(&took, FG));
        let calls = usage["calls"].as_i64().unwrap_or(0);
        if calls > 0 {
            if self.quiet() {
                let tok = usage["completion_tokens"].as_i64().unwrap_or(0);
                line.push_str(&paint(&if tok >= 1000 { format!("  ·  ↓ {:.1}k tokens", tok as f64 / 1000.0) } else { format!("  ·  ↓ {tok} tokens") }, MUTED));
            } else {
                let (p, c, hit) = (usage["prompt_tokens"].as_f64().unwrap_or(0.0), usage["cached_tokens"].as_f64().unwrap_or(0.0), usage["cache_hit_pct"].as_f64().unwrap_or(0.0));
                line.push_str(&paint(&format!("  ·  {calls} call{}  ·  {:.1}k in ", if calls != 1 { "s" } else { "" }, p / 1000.0), MUTED));
                line.push_str(&paint(&format!("({hit:.0}% cached, {:.1}k new)", (p - c) / 1000.0), if hit >= 50.0 { MINT } else { AMBER }));
                line.push_str(&paint(&format!("  ·  {:.1}k out", usage["completion_tokens"].as_f64().unwrap_or(0.0) / 1000.0), MUTED));
            }
            if let Some(c) = usage["est_cost_usd"].as_f64() {
                line.push_str(&paint(&format!("  ·  ${c}"), AMBER));
            }
        }
        self.println(&line);
    }

    /// The approval panel for a pending action; its header counts as the call's header.
    pub fn show_confirmation(&self, c: &ToolConfirmation) -> (String, String, String) {
        let cwd = self.cwd();
        let (title, q, always, body): (String, &str, &str, String);
        let args = c.params.as_object().cloned().unwrap_or_default();
        if let Some(cmd) = &c.command {
            title = "Run command".into();
            q = "Do you want to run this command?";
            always = "Yes, and don't ask again for this kind of command this session";
            body = format!("  {}", paint(cmd, LAVENDER));
        } else if let Some(d) = &c.diff {
            title = c.description.clone();
            q = "Do you want to make this edit?";
            always = "Yes, and don't ask again for edits this session";
            body = render_diff(&d.to_diff(), 40, term_width().min(120).saturating_sub(8)).0.join("\n");
        } else {
            title = c.tool_name.clone();
            q = "Do you want to proceed?";
            always = "Yes, and don't ask again this session";
            body = format!("  {}", c.description);
        }
        let danger = if c.command.is_some() { AMBER } else { LAVENDER };
        self.println(&format!("\n{}", header_text(&c.tool_name, &args, &cwd)));
        self.st.lock().unwrap().header_done = true;
        self.println(&format!("{}\n{body}", bold_paint(&format!("╭ {}", title.lines().next().unwrap_or("")), danger)));
        (q.into(), always.into(), title)
    }

    pub fn approved(&self, c: &ToolConfirmation) {
        let cwd = self.cwd();
        self.println(&paint("  ▸ approved, running…", MINT));
        self.set_phase(&format!("Running {}", tool_label(&c.tool_name, c.params.as_object().unwrap_or(&Map::new()), &cwd)));
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[allow(dead_code)]
pub fn is_quiet_command(cmd: &str) -> bool {
    cmd.split_whitespace().next().map_or(false, |c| QUIET_COMMANDS.contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn labels_and_summaries() {
        let cwd = Path::new("/w");
        assert_eq!(tool_parts("read_file", &args(json!({"path": "/w/src/a.py"})), cwd), ("Read".into(), "src/a.py".into()));
        assert_eq!(tool_label("shell", &args(json!({"command": "ls\n-la"})), cwd), "Bash(ls -la)");
        assert_eq!(tool_parts("subagent_codebase_investigator", &args(json!({"goal": "find x"})), cwd).0, "Scout agent");
        let md = args(json!({"total_lines": 100, "shown_start": 1, "shown_end": 40}));
        assert_eq!(summarize_result("read_file", true, "", None, &md, None, &Map::new()).0, "Read 40 lines (of 100)");
        let (s, p) = summarize_result("grep", true, "a:1:x\nb:2:y", None, &args(json!({"matches": 2, "files_with_matches": 2})), None, &Map::new());
        assert_eq!((s.as_str(), p.len()), ("Found 2 matches in 2 files", 2));
        assert!(summarize_result("shell", false, "", Some("boom\nmore"), &Map::new(), Some(1), &Map::new()).0.starts_with("✗ boom"));
    }

    #[test]
    fn diff_rows() {
        let d = "--- a\n+++ b\n@@ -1,2 +1,2 @@\n keep\n-old\n+new\n";
        let (rows, a, r) = parse_unified_diff(d);
        assert_eq!((a, r, rows.len()), (1, 1, 3));
        assert_eq!(rows[1], ('-', Some(2), "old".into()));
        let (lines, _, _) = render_diff(d, 2, 40);
        assert_eq!(lines.len(), 3); // two rows plus the "more" note
    }
}
