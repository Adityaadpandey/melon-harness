//! Teach melon from the Claude Code sessions you already ran in a repo. Claude Code keeps every session as a jsonl file
//! under ~/.claude/projects/<cwd with / as ->/, but deletes them after 30 days. What it never deletes is
//! ~/.claude/history.jsonl: every prompt you typed, with its folder and session id. So there are two sources: full
//! transcripts (recent) and prompt history (all time).
//!
//! Each message you typed starts a task, and the task runs until your next message. Every task gets a reward:
//!   -1  you interrupted it, rejected one of its tool calls, or your next message corrects it ("no, ...", "still broken")
//!   +1  your next message moves on ("commit it", "works", "nice")
//!    0  anything else
//! Tasks that were not punished go into melon's edit history, so `search` finds those files faster next time. The rules
//! you stated along the way become lessons, saved in melon's store and in the repo's AGENTS.md.
//!
//!     melon learn-import --repo ~/Devlopment/tunyt                   # quick dry run
//!     melon learn-import --repo ~/Devlopment/tunyt --deep            # deep dry run: prints adds and drops
//!     melon learn-import --repo ~/Devlopment/tunyt --deep --reweigh  # same candidates, new verdict (cheap)
//!     melon learn-import --repo ~/Devlopment/tunyt --deep --apply    # writes the verdict you just reviewed
use crate::agent::learn::{Learned, HEADING};
use crate::client::llm_client::LLMClient;
use crate::client::response::StreamEvent;
use crate::config::loader::{load_config, LoadOptions};
use crate::util::paths::resolve;
use crate::util::re::{lit, lit_i, Re};
use clap::Parser;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];
const INTERRUPTED: &str = "[Request interrupted by user";
const NOT_TYPED: &[&str] = &["<", "/", "This session is being continued", "Caveat:"]; // slash commands, compaction summaries, hook notes
const MAX_NEW_LESSONS: usize = 15;
const MAX_LESSONS: usize = 30;
const CHUNK_CHARS: usize = 80_000; // about 20k tokens of task stories per model call
const PARALLEL: usize = 3;

fn correction() -> &'static Re {
    // a reply that opens like this means the last task went wrong
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"^\W*(no\b|nope|wrong|revert|undo|that'?s not|not working|doesn'?t work|still (broken|not|fails|failing|the same)|it broke|you broke|why did you|stop\b)"))
}
fn approval() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"^\W*(ok\w*|okay|nice|great|perfect|works|it works|good|cool|thanks|done|commit|push|ship)\b"))
}
fn paste() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"(?s)<pasted_content[^>]*>.*?</pasted_content>|\[Pasted text #\d+[^\]]*\]"))
}

#[derive(Clone, Debug, Default)]
pub struct Task {
    pub request: String,
    pub files: Vec<String>,
    pub neg: bool,
    pub said: String,
    pub failed: Vec<String>,
    pub reply: String,
    pub reward: i32,
}

fn new_task(request: &str) -> Task {
    Task { request: paste().replace_all(request, " ").trim().to_string(), ..Default::default() }
}

/// Claude Code folders for the repo and its subfolders: /a/tunyt -> -a-tunyt, -a-tunyt-apps-web.
pub fn project_dirs(repo: &Path, projects: &Path) -> Vec<PathBuf> {
    let key = repo.to_string_lossy().replace('/', "-");
    let mut v: Vec<PathBuf> = std::fs::read_dir(projects).map(|rd| rd.flatten().map(|e| e.path()).filter(|d| d.is_dir() && d.file_name().map_or(false, |n| { let n = n.to_string_lossy(); n == key || n.starts_with(&format!("{key}-")) })).collect()).unwrap_or_default();
    v.sort();
    v
}

fn texts(content: &Value) -> Vec<String> {
    match content {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().filter(|b| b["type"] == "text").map(|b| b["text"].as_str().unwrap_or("").to_string()).collect(),
        _ => vec![],
    }
}

/// Your next message is the verdict on the task before it.
fn score(tasks: &mut [Task]) {
    for i in 0..tasks.len() {
        let reply = tasks.get(i + 1).map(|t| t.request.clone()).unwrap_or_default();
        let t = &mut tasks[i];
        t.reward = if t.neg || correction().is_match(&reply) { -1 } else if approval().is_match(&reply) { 1 } else { 0 };
        t.reply = reply;
    }
}

fn one_line(text: &str, n: usize) -> String {
    crate::util::text::squash(text).chars().take(n).collect()
}

/// One session's records -> tasks plus how often each hook block fired.
pub fn episodes(lines: &[&str], repo: &Path) -> (Vec<Task>, BTreeMap<String, usize>) {
    let (mut tasks, mut hooks): (Vec<Task>, BTreeMap<String, usize>) = (vec![], BTreeMap::new());
    let mut commands: BTreeMap<String, String> = BTreeMap::new(); // tool_use id -> the shell command, so a failed result can name it
    for line in lines {
        let Ok(r) = serde_json::from_str::<Value>(line) else { continue };
        if r["isSidechain"] == true || r["isMeta"] == true {
            continue; // subagent chatter and injected notes are not you talking
        }
        let content = &r["message"]["content"];
        match r["type"].as_str() {
            Some("user") => {
                for b in content.as_array().into_iter().flatten() {
                    if b["type"] == "tool_result" && !tasks.is_empty() {
                        if r["toolDenialKind"] == "user-rejected" {
                            tasks.last_mut().unwrap().neg = true;
                        }
                        let text = b["content"].as_str().unwrap_or("");
                        if let Some((_, rest)) = text.split_once("hook error:") {
                            *hooks.entry(rest.trim().chars().take(200).collect()).or_default() += 1;
                        } else if b["is_error"] == true {
                            if let Some(cmd) = commands.get(b["tool_use_id"].as_str().unwrap_or("")) {
                                tasks.last_mut().unwrap().failed.push(format!("{} -> {}", cmd.chars().take(120).collect::<String>(), one_line(text, 160)));
                            }
                        }
                    }
                }
                for t in texts(content) {
                    if t.starts_with(INTERRUPTED) {
                        if let Some(l) = tasks.last_mut() {
                            l.neg = true;
                        }
                    } else if !t.trim().is_empty() && !NOT_TYPED.iter().any(|p| t.trim_start().starts_with(p)) {
                        tasks.push(new_task(&t));
                    }
                }
            }
            Some("assistant") if content.is_array() && !tasks.is_empty() => {
                for b in content.as_array().unwrap() {
                    if b["type"] == "text" && !b["text"].as_str().unwrap_or("").trim().is_empty() {
                        tasks.last_mut().unwrap().said = b["text"].as_str().unwrap().into();
                    }
                    if b["type"] != "tool_use" {
                        continue;
                    }
                    let args = &b["input"];
                    let name = b["name"].as_str().unwrap_or("");
                    if name == "Bash" {
                        commands.insert(b["id"].as_str().unwrap_or("").into(), crate::util::text::squash(args["command"].as_str().unwrap_or("")));
                    }
                    if EDIT_TOOLS.contains(&name) {
                        let path = args["file_path"].as_str().or(args["notebook_path"].as_str()).unwrap_or("");
                        let Ok(rel) = resolve(Path::new(path)).strip_prefix(repo).map(|r| r.to_string_lossy().to_string()) else { continue }; // a scratch file or another repo
                        let t = tasks.last_mut().unwrap();
                        if !t.files.contains(&rel) {
                            t.files.push(rel);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    score(&mut tasks);
    (tasks, hooks)
}

/// Every task from the transcripts still on disk, the hook counts, and which session ids those were.
pub fn collect(repo: &Path, projects: &Path) -> (Vec<Task>, BTreeMap<String, usize>, Vec<String>) {
    let (mut tasks, mut hooks, mut seen) = (vec![], BTreeMap::new(), vec![]);
    for d in project_dirs(repo, projects) {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&d).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "jsonl")).collect()).unwrap_or_default();
        files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok()); // oldest first, like they happened
        for f in files {
            let text = String::from_utf8_lossy(&std::fs::read(&f).unwrap_or_default()).to_string();
            let lines: Vec<&str> = text.lines().collect();
            let (t, h) = episodes(&lines, repo);
            tasks.extend(t);
            for (k, v) in h {
                *hooks.entry(k).or_default() += v;
            }
            seen.push(f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
        }
    }
    (tasks, hooks, seen)
}

/// Tasks from the prompt history, for sessions whose transcript Claude Code already deleted.
pub fn history_tasks(repo: &Path, history: &Path, skip: &[String]) -> Vec<Task> {
    let Ok(text) = std::fs::read_to_string(history) else { return vec![] };
    let mut sessions: BTreeMap<String, Vec<(i64, String)>> = BTreeMap::new();
    let repo_s = repo.to_string_lossy().to_string();
    for line in text.lines() {
        let Ok(r) = serde_json::from_str::<Value>(line) else { continue };
        let (project, sid, t) = (r["project"].as_str().unwrap_or(""), r["sessionId"].as_str().unwrap_or(""), r["display"].as_str().unwrap_or(""));
        if skip.iter().any(|s| s == sid) || t.trim().is_empty() || NOT_TYPED.iter().any(|p| t.trim_start().starts_with(p)) {
            continue;
        }
        if project == repo_s || project.starts_with(&format!("{repo_s}/")) {
            sessions.entry(sid.to_string()).or_default().push((r["timestamp"].as_i64().unwrap_or(0), t.to_string()));
        }
    }
    let mut ordered: Vec<Vec<(i64, String)>> = sessions.into_values().collect();
    ordered.iter_mut().for_each(|p| p.sort());
    ordered.sort_by_key(|p| p[0].0);
    let mut tasks = vec![];
    for prompts in ordered {
        let mut t: Vec<Task> = prompts.iter().map(|(_, text)| new_task(text)).collect();
        score(&mut t);
        tasks.extend(t);
    }
    tasks
}

/// One task as a few lines a model can read. Punished tasks get more room, since that's where the lessons are.
pub fn story(t: &Task) -> String {
    let room = if t.reward < 0 { 900 } else { 500 };
    let mut out = vec![format!("[{}] YOU: {}", if t.reward > 0 { "+1".to_string() } else { t.reward.to_string() }, one_line(&t.request, room))];
    if !t.files.is_empty() {
        out.push(format!("  edited: {}", t.files.iter().take(6).cloned().collect::<Vec<_>>().join(", ")));
    }
    out.extend(t.failed.iter().take(3).map(|f| format!("  failed: {f}")));
    if !t.said.is_empty() {
        out.push(format!("  CLAUDE: {}", one_line(&t.said, if t.reward < 0 { 400 } else { 200 })));
    }
    if t.reward < 0 && !t.reply.is_empty() {
        out.push(format!("  YOU NEXT: {}", one_line(&t.reply, 400)));
    }
    out.join("\n")
}

pub fn chunks(tasks: &[Task], size: usize) -> Vec<String> {
    let (mut out, mut cur) = (vec![], String::new());
    for t in tasks.iter().filter(|t| !t.request.is_empty()) {
        let s = format!("{}\n", story(t));
        if !cur.is_empty() && cur.len() + s.len() > size {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(&s);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// What the repo already tells agents: every AGENTS.md (minus melon's own section, which is under review), Command Code
/// taste, and each skill's name and description.
pub(crate) fn rulebook(repo: &Path) -> String {
    let mut parts = vec![];
    let mut agents: Vec<PathBuf> = walkdir::WalkDir::new(repo).into_iter().filter_entry(|e| !matches!(e.file_name().to_string_lossy().as_ref(), "node_modules" | ".git" | ".deepsec")).flatten().filter(|e| e.file_name() == "AGENTS.md").map(|e| e.into_path()).collect();
    agents.sort();
    for f in agents {
        let body = std::fs::read_to_string(&f).unwrap_or_default();
        parts.push(format!("=== {}\n{}", f.strip_prefix(repo).unwrap_or(&f).display(), body.split(HEADING).next().unwrap_or("").trim()));
    }
    let taste = repo.join(".commandcode/taste");
    let mut tf: Vec<PathBuf> = walkdir::WalkDir::new(&taste).into_iter().flatten().filter(|e| e.path().extension().map_or(false, |x| x == "md")).map(|e| e.into_path()).collect();
    tf.sort();
    for f in tf {
        parts.push(format!("=== {}\n{}", f.strip_prefix(repo).unwrap_or(&f).display(), std::fs::read_to_string(&f).unwrap_or_default().trim()));
    }
    let mut skills = vec![];
    let mut sf: Vec<PathBuf> = std::fs::read_dir(repo.join(".claude/skills")).map(|rd| rd.flatten().map(|e| e.path().join("SKILL.md")).filter(|p| p.is_file()).collect()).unwrap_or_default();
    sf.sort();
    let mut docs: Vec<PathBuf> = std::fs::read_dir(repo.join("docs/skills")).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x == "md")).collect()).unwrap_or_default();
    docs.sort();
    sf.extend(docs);
    for f in sf {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        let desc = lit(r"(?m)^description:\s*(.+)$").captures(&text).and_then(|c| c[1].clone()).map(|d| d.trim().chars().take(200).collect::<String>()).unwrap_or_default();
        skills.push(format!("- {}: {desc}", f.strip_prefix(repo).unwrap_or(&f).display()));
    }
    if !skills.is_empty() {
        parts.push(format!("=== skills\n{}", skills.join("\n")));
    }
    parts.join("\n\n")
}

async fn ask(system: &str, user: &str, client: &Arc<LLMClient>) -> Result<String, String> {
    let mut out = String::new();
    let mut s = client.chat_completion(vec![json!({"role": "system", "content": system}), json!({"role": "user", "content": user})], None, false, "plan");
    while let Some(ev) = s.next().await {
        match ev {
            StreamEvent::MessageComplete { text: Some(t), .. } => out.push_str(&t),
            StreamEvent::Error(e) => return Err(format!("model call failed: {e}")),
            _ => {}
        }
    }
    Ok(out)
}

fn extract_json(out: &str, open: char, close: char) -> Result<Value, String> {
    let (s, e) = (out.find(open), out.rfind(close));
    match (s, e) {
        (Some(s), Some(e)) if e > s => serde_json::from_str(&out[s..=e]).map_err(|_| format!("model did not return JSON:\n{}", out.chars().take(500).collect::<String>())),
        _ => Err(format!("model did not return JSON:\n{}", out.chars().take(500).collect::<String>())),
    }
}

const LESSON_PROMPT: &str = "You read what a developer typed to a coding agent while working in one repository, oldest first.
Pull out the STANDING rules: preferences that should hold for every future task in this repo, such as how to
commit, which tools or commands to use, what never to touch, or how they want work checked.
Skip one-off task instructions (\"fix the login bug\", \"add a refund button\") and anything only true for that day.
A rule they repeated, or corrected the agent about, is the strongest evidence. \"Hook blocks\" lists repo hooks
that stopped the agent, with how often each fired.
Write each rule as one short imperative line (max 25 words) in plain English. Fix their typos.
Leave out anything already in \"Already known\". Return at most {n} rules as a JSON array of strings, nothing else.";

/// The text the model reads. Long briefs and pasted docs are cut short, since rules are said in a line.
pub(crate) fn evidence(tasks: &[Task], hooks: &BTreeMap<String, usize>, known: &[String]) -> String {
    let mut said: Vec<String> = vec![];
    for t in tasks.iter().filter(|t| !t.request.is_empty()) {
        let l = one_line(&t.request, 300);
        if !said.contains(&l) {
            said.push(l);
        }
    }
    let corrected: Vec<String> = tasks.iter().filter(|t| t.reward < 0 && !t.reply.is_empty()).map(|t| one_line(&t.reply, 300)).collect();
    let mut parts = vec![format!("Messages:\n{}", said.iter().map(|m| format!("- {m}")).collect::<Vec<_>>().join("\n"))];
    if !corrected.is_empty() {
        parts.push(format!("Replies right after the agent got it wrong:\n{}", corrected.iter().map(|m| format!("- {m}")).collect::<Vec<_>>().join("\n")));
    }
    if !hooks.is_empty() {
        parts.push(format!("Hook blocks:\n{}", most_common(hooks, 10).iter().map(|(h, n)| format!("- ({n}x) {h}")).collect::<Vec<_>>().join("\n")));
    }
    if !known.is_empty() {
        parts.push(format!("Already known:\n{}", known.iter().map(|k| format!("- {k}")).collect::<Vec<_>>().join("\n")));
    }
    parts.join("\n\n")
}

fn most_common(m: &BTreeMap<String, usize>, n: usize) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = m.iter().map(|(k, c)| (k.clone(), *c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(n);
    v
}

const MAP_PROMPT: &str = "You read stories of tasks a developer gave a coding agent in one repository, oldest first.
Each starts with the reward: [-1] the developer interrupted, rejected or corrected the agent, [+1] they moved on
happily, [0] no signal. YOU is the developer, CLAUDE is the agent's last words, \"failed\" are commands that errored,
YOU NEXT is the developer's reply after a punished task.
Find what an agent should know before its next task here:
  * standing preferences the developer stated or kept enforcing (how to commit, what to ask first, scope, tone)
  * mistakes the agent made more than once, or that the developer corrected sharply, and what to do instead
  * repo gotchas: a command that fails and the one that works, a file that is the real source of truth
Skip one-off feature requests and anything only true that day. Be concrete: name commands, files, paths.
Return a JSON array of objects {\"rule\": \"<one imperative line, max 30 words>\", \"evidence\": \"<short quote or count>\"}.
At most {n} items, the best supported first. Nothing else.";

const REDUCE_PROMPT: &str = "You maintain the \"Learned by melon\" section of a repository's AGENTS.md: rules an agent learned
from working with this developer. You get:
  * RULEBOOK: what the repo already tells agents (AGENTS.md files, learned taste, skills)
  * CURRENT: the numbered lessons in the section now
  * CANDIDATES: rules mined from hundreds of the developer's past sessions, with evidence
Decide the final section:
  * drop a CURRENT lesson that the RULEBOOK already says (even in other words, and above all anything the
    Absolute Rules cover), that is vague, that is a fragment or an error message rather than a rule, or that a
    better candidate replaces. Keeping a duplicate of the rulebook is a mistake: the agent reads both.
  * add a CANDIDATE only if the RULEBOOK does not already say it, it is supported by real evidence, and it will
    still be true next month. Merge candidates that say the same thing. Prefer rules with concrete commands/paths.
  * a lesson is about how an agent should WORK in this repo (asking, scope, verifying, committing, commands,
    gotchas). How a product feature should behave (\"the event AI asks one question at a time\") is a product
    requirement for that feature's spec, not a lesson: leave it out.
  * never add anything that contradicts the RULEBOOK
The section may hold at most {cap} lessons after your changes. Each new lesson is one plain English line, max 30 words.
Return JSON {\"drop\": [<CURRENT numbers>], \"add\": [\"<lesson>\", ...], \"why\": {\"<lesson or number>\": \"<few words>\"}}.
Nothing else.";

/// Mine candidates from every task (skipped when `found` is handed in from a saved run), then weigh them.
async fn deep(tasks: &[Task], hooks: &BTreeMap<String, usize>, repo: &Path, learned: &Learned, found: Option<Vec<Value>>) -> Result<Value, String> {
    let mut parts = chunks(tasks, CHUNK_CHARS);
    if !hooks.is_empty() {
        parts.push(format!("Hook blocks that stopped the agent:\n{}", most_common(hooks, 10).iter().map(|(h, n)| format!("- ({n}x) {h}")).collect::<Vec<_>>().join("\n")));
    }
    let cfg = load_config(Some(repo), &LoadOptions { trust_project: false, pi_mode: None })?;
    let tracer = Arc::new(crate::util::trace::Tracer::new("learn-import", None));
    let client = Arc::new(LLMClient::new(crate::config::ConfigHandle::new(cfg), tracer));
    client.initialize().await;
    let found = match found {
        Some(f) => f,
        None => {
            println!("mining {} chunks ({} tokens)", parts.len(), parts.iter().map(String::len).sum::<usize>() / 4);
            let gate = Arc::new(tokio::sync::Semaphore::new(PARALLEL));
            let total = parts.len();
            let futs = parts.iter().enumerate().map(|(i, text)| {
                let (gate, client) = (gate.clone(), client.clone());
                async move {
                    let _p = gate.acquire().await;
                    let got = extract_json(&ask(&MAP_PROMPT.replace("{n}", "12"), text, &client).await?, '[', ']')?;
                    let items: Vec<Value> = got.as_array().cloned().unwrap_or_default().into_iter().filter(|c| c["rule"].as_str().map_or(false, |r| !r.is_empty())).collect();
                    println!("  chunk {}/{total}: {} candidates", i + 1, items.len());
                    Ok::<Vec<Value>, String>(items)
                }
            });
            let mut all = vec![];
            for r in futures::future::join_all(futs).await {
                all.extend(r?);
            }
            all
        }
    };
    let current = learned.lessons();
    let user = [
        format!("RULEBOOK:\n{}", rulebook(repo)),
        format!("CURRENT:\n{}", current.iter().enumerate().map(|(i, t)| format!("{}. {t}", i + 1)).collect::<Vec<_>>().join("\n")),
        format!("CANDIDATES:\n{}", found.iter().map(|c| format!("- {} (evidence: {})", c["rule"].as_str().unwrap_or(""), one_line(&c["evidence"].to_string().trim_matches('"').to_string(), 160))).collect::<Vec<_>>().join("\n")),
    ]
    .join("\n\n");
    println!("weighing {} candidates against the rulebook ({} tokens)", found.len(), user.len() / 4);
    let verdict = extract_json(&ask(&REDUCE_PROMPT.replace("{cap}", &MAX_LESSONS.to_string()), &user, &client).await?, '{', '}')?;
    let mut drop: Vec<usize> = verdict["drop"].as_array().into_iter().flatten().filter_map(Value::as_u64).map(|n| n as usize).filter(|n| *n >= 1 && *n <= current.len()).collect();
    drop.sort();
    drop.dedup();
    let room = MAX_LESSONS.saturating_sub(current.len() - drop.len());
    let add: Vec<&str> = verdict["add"].as_array().into_iter().flatten().filter_map(Value::as_str).take(room).collect();
    Ok(json!({"drop": drop.iter().map(|n| current[n - 1].clone()).collect::<Vec<_>>(), "add": add, "why": verdict["why"], "candidates": found}))
}

#[derive(Parser, Debug)]
#[command(name = "melon learn-import", about = "Teach melon from the Claude Code sessions you already ran in a repo")]
struct Args {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    projects: Option<PathBuf>,
    #[arg(long)]
    history: Option<PathBuf>,
    /// write to melon's store and AGENTS.md (default: dry run)
    #[arg(long)]
    apply: bool,
    /// edit history only, skip the model calls
    #[arg(long = "no-lessons")]
    no_lessons: bool,
    /// read every session's story and check lessons against the rulebook
    #[arg(long)]
    deep: bool,
    /// with --deep: reuse the saved candidates, redo only the final verdict
    #[arg(long)]
    reweigh: bool,
}

pub async fn main(argv: Vec<String>) -> i32 {
    let args = Args::parse_from(argv);
    let home = dirs::home_dir().unwrap_or_default();
    let repo = resolve(&crate::util::paths::expanduser(&args.repo.to_string_lossy()));
    let projects = args.projects.clone().unwrap_or_else(|| home.join(".claude/projects"));
    let history = args.history.clone().unwrap_or_else(|| home.join(".claude/history.jsonl"));

    let (tasks, hooks, seen) = collect(&repo, &projects);
    let old = if args.deep { history_tasks(&repo, &history, &seen) } else { vec![] };
    let count = |r: i32| tasks.iter().chain(old.iter()).filter(|t| t.reward == r).count();
    let kept: Vec<&Task> = tasks.iter().filter(|t| t.reward >= 0 && !t.files.is_empty()).collect();
    println!("{} transcripts ({} tasks) + {} prompts from deleted sessions: {} rewarded, {} neutral, {} punished; {} unpunished tasks edited files", seen.len(), tasks.len(), old.len(), count(1), count(0), count(-1), kept.len());

    let mut learned = Learned::new(&repo);
    if args.apply {
        let before = learned.edit_count();
        for t in &kept {
            learned.record_run(&t.request, &t.files); // drops files that are gone, skips repeats, keeps the newest 200
        }
        println!("edit history: {before} -> {}", learned.edit_count());
    }
    if args.no_lessons {
        return 0;
    }
    if args.deep {
        // the dry run is saved next to melon's store, so --apply writes exactly what you reviewed (no new model call)
        // and --reweigh redoes only the last, cheap step on the same candidates
        let saved_path = learned.store_path().with_extension("deep.json");
        let saved: Option<Value> = std::fs::read_to_string(&saved_path).ok().and_then(|t| serde_json::from_str(&t).ok());
        let result = if args.apply && saved.is_some() && !args.reweigh {
            println!("applying the dry run saved in {}", saved_path.display());
            saved.unwrap()
        } else {
            let reuse = if args.reweigh { saved.as_ref().and_then(|s| s["candidates"].as_array().cloned()) } else { None };
            let all: Vec<Task> = old.iter().chain(tasks.iter()).cloned().collect(); // oldest first
            match deep(&all, &hooks, &repo, &learned, reuse).await {
                Ok(r) => {
                    let _ = std::fs::write(&saved_path, serde_json::to_string_pretty(&r).unwrap_or_default());
                    r
                }
                Err(e) => {
                    eprintln!("{e}");
                    return 1;
                }
            }
        };
        for t in result["drop"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            println!("- {}", t.chars().take(160).collect::<String>());
        }
        for t in result["add"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            println!("+ {t}");
        }
        if args.apply {
            for t in result["drop"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if let Some(i) = learned.lessons().iter().position(|l| l == t) {
                    learned.forget(i + 1);
                }
            }
            let added = result["add"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|t| learned.add_lesson(t, "claude-code")).count();
            let _ = std::fs::remove_file(&saved_path); // used up: the next run looks at the lessons fresh
            println!("dropped {}, added {added}: {} lessons now", result["drop"].as_array().map_or(0, Vec::len), learned.lessons().len());
        } else {
            println!("{} candidates and this verdict saved to {}", result["candidates"].as_array().map_or(0, Vec::len), saved_path.display());
        }
    } else {
        let cfg = match load_config(Some(&repo), &LoadOptions { trust_project: false, pi_mode: None }) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        };
        let client = Arc::new(LLMClient::new(crate::config::ConfigHandle::new(cfg), Arc::new(crate::util::trace::Tracer::new("learn-import", None))));
        client.initialize().await;
        let reply = match ask(&LESSON_PROMPT.replace("{n}", &MAX_NEW_LESSONS.to_string()), &evidence(&tasks, &hooks, &learned.lessons()), &client).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        };
        let rules = match extract_json(&reply, '[', ']') {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        };
        for r in rules.as_array().into_iter().flatten().filter_map(Value::as_str).take(MAX_NEW_LESSONS) {
            let mark = if args.apply { if learned.add_lesson(r, "claude-code") { "+ " } else { "= " } } else { "? " };
            println!("{mark}{r}");
        }
    }
    if !args.apply {
        println!("dry run: nothing written. Add --apply to save it.");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewards_follow_the_next_message() {
        let mut t = vec![new_task("add a button"), new_task("no, not that one"), new_task("works, commit it")];
        score(&mut t);
        assert_eq!(t.iter().map(|x| x.reward).collect::<Vec<_>>(), vec![-1, 1, 0]);
    }

    #[test]
    fn episodes_read_transcripts() {
        let repo = Path::new("/w/repo");
        let lines = [
            r#"{"type":"user","message":{"content":"fix the login bug"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"on it"},{"type":"tool_use","name":"Edit","id":"1","input":{"file_path":"/w/repo/src/a.py"}}]}}"#,
            r#"{"type":"user","message":{"content":"[Request interrupted by user for tool use]"}}"#,
            r#"{"type":"user","message":{"content":"still broken, revert it"}}"#,
        ];
        let (tasks, _) = episodes(&lines, repo);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].files, vec!["src/a.py"]);
        assert!(tasks[0].neg && tasks[0].reward == -1);
        assert!(story(&tasks[0]).contains("YOU NEXT: still broken"));
    }
}
