//! Ported from tests/test_tui.py
//! Skipped (Python-only prompt_toolkit/Rich machinery, or not in the Rust port):
//!   - ask_choice single keypress (prompt_toolkit pipe input; Rust reads keys from the terminal directly)
//!   - confirm returns the user's choice (the Rust prompt waits on a real tty; show_confirmation is tested instead)
//!   - a question cancelled mid-wait is forgotten (prompt_toolkit futures)
//!   - resume replays the whole conversation (the Rust Tui has no replay)
use super::SERIAL;
use crate::tools::FileDiff;
use crate::ui::app::expand_mentions;
use crate::ui::editor::Editor;
use crate::ui::input::Completions;
use crate::ui::md::split_blocks;
use crate::ui::screen::{Action, Screen};
use crate::ui::sink;
use crate::ui::style::{context_bar, strip_ansi};
use crate::ui::tui::{parse_unified_diff, render_diff, summarize_result, tool_label, Tui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn m(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn diff() -> String {
    FileDiff { path: "a.py".into(), old_content: "x = 1\ny = 2\nz = 3\n".into(), new_content: "x = 1\ny = 20\nz = 3\nw = 4\n".into(), is_new_file: false, is_deletion: false }.to_diff()
}

/// A Tui whose output is caught in the sink; `text()` gives back what it printed, without colours.
struct Rig {
    tui: Arc<Tui>,
}
impl Rig {
    fn new(dir: &Path) -> Rig {
        sink::install();
        Rig { tui: Tui::new(dir) }
    }
    fn text(&self) -> String {
        strip_ansi(&sink::with(|s| s.text.clone()).unwrap_or_default())
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        sink::uninstall();
    }
}

#[test]
fn tool_labels_are_short_and_readable() {
    let dir = Path::new("/w");
    assert_eq!(tool_label("read_file", &m(json!({"path": "/w/src/a.py", "limit": 50})), dir), "Read(src/a.py)");
    assert_eq!(tool_label("shell", &m(json!({"command": "pnpm test\n--filter x"})), dir), "Bash(pnpm test --filter x)");
    assert_eq!(tool_label("grep", &m(json!({"pattern": "load", "glob": "*.ts"})), dir), "Search(load, *.ts)");
    assert_eq!(tool_label("subagent_debugger", &m(json!({"goal": "why 500"})), dir), "Debugger agent(why 500)");
    assert_eq!(tool_label("mystery", &m(json!({"n": 3})), dir), "mystery(3)"); // non-string args must not crash
}

#[test]
fn diff_line_numbers_counts_and_truncation() {
    let (rows, adds, dels) = parse_unified_diff(&diff());
    assert_eq!((adds, dels), (2, 1));
    assert!(rows.contains(&('-', Some(2), "y = 2".into())) && rows.contains(&('+', Some(2), "y = 20".into())) && rows.contains(&('+', Some(4), "w = 4".into())));
    let big = FileDiff { path: "b".into(), old_content: String::new(), new_content: (0..100).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n") + "\n", is_new_file: false, is_deletion: false }.to_diff();
    let (lines, adds, _) = render_diff(&big, 10, 80);
    assert_eq!(adds, 100);
    assert!(strip_ansi(&lines.join("\n")).contains("+90 more changed lines"));
}

#[test]
fn result_summaries() {
    let none = Map::new();
    assert_eq!(summarize_result("read_file", true, "", None, &m(json!({"shown_start": 1, "shown_end": 40, "total_lines": 400})), None, &none).0, "Read 40 lines (of 400)");
    assert_eq!(summarize_result("grep", true, "a:1:x\nb:2:y", None, &m(json!({"matches": 2, "files_with_matches": 2})), None, &none).0, "Found 2 matches in 2 files");
    let out: String = (0..20).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
    let (s, pre) = summarize_result("shell", true, &out, None, &none, Some(0), &none);
    assert_eq!(s, "Ran");
    assert_eq!(pre.last().unwrap(), "… +14 lines");
    let (s, _) = summarize_result("edit", false, "", Some("old_string not found.\nClosest lines:"), &none, None, &none);
    assert!(s.starts_with("✗ old_string not found"));
}

#[tokio::test]
async fn transcript_shape() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let r = Rig::new(dir.path());
    let t = &r.tui;
    t.toggle_quiet(); // the full layout; quiet mode has its own test below
    let a_py = dir.path().join("a.py").to_string_lossy().to_string();
    t.tool_call_start("c1", "edit", &m(json!({"path": a_py, "old_string": "y = 2", "new_string": "y = 20"})));
    t.tool_call_complete("c1", "edit", true, "Edited a.py", None, &m(json!({"path": a_py})), Some(&diff()), None);
    // a parallel batch: both start, then both complete; each result stays under its own header
    t.tool_call_start("c2", "grep", &m(json!({"pattern": "needle"})));
    t.tool_call_start("c3", "read_file", &m(json!({"path": "b.py"})));
    t.tool_call_complete("c2", "grep", true, "x:1:needle", None, &m(json!({"matches": 1, "files_with_matches": 1})), None, None);
    t.tool_call_complete("c3", "read_file", true, "", None, &m(json!({"shown_start": 1, "shown_end": 5, "total_lines": 5})), None, None);
    t.tool_call_start("c4", "shell", &m(json!({"command": "false"})));
    t.tool_call_complete("c4", "shell", false, "boom", Some("Exit code 1"), &Map::new(), None, Some(1));
    let out = r.text();
    let re = |p: &str| regex::Regex::new(p).unwrap().is_match(&out);
    assert!(re(r"\[search\]\s+needle\n.*✓ Found 1 match.*\n.*x:1:needle\n\n.*\[read\]\s+b.py\n.*✓ Read 5 lines"), "{out}");
    assert!(re(r"\[update\]\s+a.py"), "{out}");
    assert!(re(r"updated\s+\+2 −1"), "{out}");
    assert!(re(r"2 \+ y = 20") && re(r"2 - y = 2"), "{out}");
    assert!(out.contains("✗ Exit code 1"));
}

#[tokio::test]
async fn show_confirmation_names_the_question_for_each_kind_of_action() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let r = Rig::new(dir.path());
    let conf = |cmd: Option<&str>, with_diff: bool| crate::tools::ToolConfirmation {
        tool_name: if cmd.is_some() { "shell" } else { "edit" }.into(),
        params: json!({"command": cmd.unwrap_or(""), "path": "a.py"}),
        description: "Edit a.py".into(),
        diff: with_diff.then(|| FileDiff { path: "a.py".into(), old_content: "a\n".into(), new_content: "b\n".into(), is_new_file: false, is_deletion: false }),
        affected_paths: vec![],
        command: cmd.map(String::from),
        is_dangerous: false,
    };
    assert_eq!(r.tui.show_confirmation(&conf(Some("ls"), false)).0, "Do you want to run this command?");
    let (q, always, title) = r.tui.show_confirmation(&conf(None, true));
    assert!(q.contains("make this edit") && always.contains("edits") && title == "Edit a.py");
    assert!(r.text().contains("Edit a.py"));
}

fn comps(c: &Completions, text: &str) -> Vec<String> {
    c.complete(text, text.len()).unwrap().1.into_iter().map(|p| p.replacement).collect()
}

#[test]
fn completer_slash_and_file_mentions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/auth.service.ts"), "x").unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
    std::process::Command::new("git").args(["init", "-q"]).current_dir(dir.path()).output().unwrap();
    let models = || vec!["claude-sonnet-5-5".to_string(), "deepseek/deepseek-v4-pro".to_string()];
    let c = Completions::new(dir.path().to_path_buf(), vec![".env".into()], Box::new(models), Arc::new(Mutex::new(vec![])));
    assert!(comps(&c, "/co").contains(&"/compact".to_string()) && !comps(&c, "/co").contains(&"/model".to_string()));
    assert_eq!(comps(&c, "explain @auth"), vec!["@src/auth.service.ts"]);
    assert!(comps(&c, "@env").is_empty()); // secrets never offered
    assert_eq!(comps(&c, "/model deep"), vec!["deepseek/deepseek-v4-pro"]);
}

#[test]
fn mentions_attach_files_but_never_secrets_or_protected_paths() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::write(p.join("a.py"), "print(1)").unwrap();
    std::fs::write(p.join(".env"), "TOKEN=abc").unwrap();
    std::fs::create_dir_all(p.join("pkg")).unwrap();
    std::fs::write(p.join("pkg/m.py"), "").unwrap();
    std::fs::write(p.join("private.txt"), "nope").unwrap();
    let (text, attached) = expand_mentions("look at @a.py and @.env and @pkg and @private.txt and @missing.py", p, &["private.*".to_string()]);
    assert_eq!(attached, vec!["a.py", "pkg"]);
    assert!(text.contains("print(1)") && !text.contains("TOKEN") && !text.contains("nope"));
    assert!(text.contains("<directory path=\"pkg\">") && text.contains("m.py"), "{text}");
}

#[test]
fn split_blocks_waits_for_complete_paragraphs_and_keeps_code_and_lists_whole() {
    let pair = |s: &str| split_blocks(s);
    assert_eq!(pair("Hello wor"), ("".into(), "Hello wor".into())); // nothing finished yet
    assert_eq!(pair("Para one.\n\nPara tw"), ("Para one.\n\n".into(), "Para tw".into())); // first paragraph is done
    let (done, rest) = pair("Intro\n\n```py\nx = 1\n\ny = 2\n```\n\nafter");
    assert_eq!((done.as_str(), rest.as_str()), ("Intro\n\n```py\nx = 1\n\ny = 2\n```\n\n", "after")); // blank line inside the fence is not a cut
    assert_eq!(pair("```py\nx = 1\n\ny = 2").0, ""); // unclosed fence: keep waiting
    assert_eq!(pair("1. one\n\n2. two\n\n3. thr").0, ""); // list items separated by blank lines stay together (numbering would restart)
    assert_eq!(pair("Intro\n\n- a\n- b\n\nOutro"), ("Intro\n\n- a\n- b\n\n".into(), "Outro".into())); // the list stays with its intro
    assert_eq!(pair("Done.\n\n").0, ""); // blank line with nothing after it yet
}

#[tokio::test]
async fn assistant_text_is_committed_as_rendered_markdown_blocks_in_order() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let r = Rig::new(dir.path());
    r.tui.start_turn("Thinking");
    for chunk in ["# Title\n\nFirst ", "paragraph.\n\n", "- item a\n- item b\n\n", "```py\nprint(1)\n```\n\nlast"] {
        r.tui.assistant_delta(chunk);
    }
    r.tui.end_turn();
    let out = r.text();
    let at = |s: &str| out.find(s).unwrap_or_else(|| panic!("{s:?} missing in {out}"));
    assert!(at("Title") < at("First paragraph.") && at("First paragraph.") < at("item a") && at("item a") < at("print(1)") && at("print(1)") < at("last"));
    assert!(!out.contains("# Title") && !out.contains("```")); // markdown was rendered, not echoed raw
    assert_eq!(out.matches('◆').count(), 1); // one marker for the whole message, not one per block
}

#[tokio::test]
async fn text_after_a_tool_call_starts_a_new_message() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let r = Rig::new(dir.path());
    r.tui.start_turn("Thinking");
    r.tui.assistant_delta("Let me look.\n\n");
    r.tui.tool_call_start("c1", "read_file", &m(json!({"path": "a.py"})));
    r.tui.tool_call_complete("c1", "read_file", true, "", None, &m(json!({"shown_start": 1, "shown_end": 3, "total_lines": 3})), None, None);
    r.tui.assistant_delta("Found it.");
    r.tui.end_turn();
    let out = r.text();
    let read_at = regex::Regex::new(r"\[read\]\s+a.py").unwrap().find(&out).unwrap().start();
    assert!(out.find("Let me look.").unwrap() < read_at && read_at < out.find("Found it.").unwrap(), "{out}");
    assert_eq!(out.matches('◆').count(), 2); // two messages around the tool call
}

#[tokio::test]
async fn live_state_todos_agents_and_phase() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let r = Rig::new(dir.path());
    let t = &r.tui;
    t.set_todos(vec![("write tests".into(), true), ("fix bug".into(), false)]);
    t.agent_progress("c1", &json!({"kind": "start", "agent": "investigator", "goal": "map it"}));
    t.agent_progress("c1", &json!({"kind": "tool", "agent": "investigator", "name": "grep", "args": {"pattern": "needle"}}));
    t.agent_progress("c1", &json!({"kind": "tool", "agent": "investigator", "name": "read_file", "args": {"path": "a.py"}}));
    assert!(t.snapshot_side().read.iter().any(|(p, _)| p == "a.py")); // a sub-agent's reads show in the READ panel too
    t.agent_progress("c1", &json!({"kind": "tool", "agent": "investigator", "name": "grep", "args": {"pattern": "needle"}}));
    t.agent_progress("c2", &json!({"kind": "start", "agent": "debugger", "goal": "why"}));
    t.start_turn("Thinking");
    let text = strip_ansi(&t.status_lines().join("\n"));
    assert!(text.contains("Scout  map it") && text.contains("Search(needle)") && !text.contains("2 tools") && text.contains("3 tools"), "{text}");
    assert!(text.contains("Debugger  why") && text.contains("starting…"), "{text}");
    assert!(text.contains("◼ write tests") && text.contains("◻ fix bug") && text.contains("Thinking…"), "{text}");
    t.agent_progress("c1", &json!({"kind": "done", "agent": "investigator", "ok": true, "tokens": 18400}));
    let side = t.snapshot_side();
    assert!(side.running.len() == 1 && side.running[0].0 == "debugger"); // c1 left the running list, c2 is still there
    let done = strip_ansi(&t.status_lines().join("\n"));
    assert!(done.contains("✓ Scout  map it") && done.contains("18k tok"), "{done}"); // a finished agent stays as one line
    t.end_turn();
    let after = t.snapshot_side();
    assert!(after.running.is_empty() && after.finished.is_empty()); // nothing lingers after the turn
}

#[tokio::test]
async fn expand_shows_full_output_of_recent_tool_calls() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let r = Rig::new(dir.path());
    let outs = [(0..30).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n"), "second call output".to_string()];
    for (i, out) in outs.iter().enumerate() {
        r.tui.tool_call_start(&format!("c{i}"), "shell", &m(json!({"command": format!("cmd{i}")})));
        r.tui.tool_call_complete(&format!("c{i}"), "shell", true, out, None, &Map::new(), None, Some(0));
    }
    sink::clear();
    r.tui.show_result(2);
    let text = r.text();
    assert!(text.contains("line 29") && text.contains("cmd0"), "{text}"); // the older call, in full (the inline view only showed 6 lines)
    r.tui.show_result(9);
    assert!(r.text().contains("no such tool result"));
}

#[tokio::test]
async fn quiet_mode_is_one_line_per_tool_call_and_hides_routine_notices() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let r = Rig::new(dir.path());
    r.tui.tool_call_start("c2", "grep", &m(json!({"pattern": "needle"})));
    r.tui.tool_call_complete("c2", "grep", true, "x:1:needle", None, &m(json!({"matches": 1, "files_with_matches": 1})), None, None);
    r.tui.notice("Loaded playbooks: code-review");
    r.tui.notice("Call budget reached (20 tool calls); wrapping up");
    let out = r.text();
    assert!(out.matches('\n').count() <= 3 && !out.contains("x:1:needle") && out.contains("Found 1 match"), "{out}");
    assert!(!out.contains("Loaded playbooks") && out.contains("Call budget reached"));
}

#[tokio::test]
async fn context_bar_and_summary() {
    let _g = SERIAL.lock().await;
    assert_eq!(context_bar(0.0, 10), format!("{} 0%", "▱".repeat(10)));
    assert_eq!(context_bar(50.0, 10), format!("{}{} 50%", "▰".repeat(5), "▱".repeat(5)));
    assert!(context_bar(100.0, 10).starts_with(&"▰".repeat(10)));
    let dir = tempfile::tempdir().unwrap();
    let r = Rig::new(dir.path());
    r.tui.toggle_quiet();
    r.tui.turn_summary(12.4, &json!({"calls": 3, "prompt_tokens": 4100, "completion_tokens": 300, "cached_tokens": 3200, "cache_hit_pct": 78.0, "est_cost_usd": null}));
    assert!(r.text().contains("12s  ·  3 calls  ·  4.1k in (78% cached, 0.9k new)  ·  0.3k out"), "{}", r.text());
}

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

/// A screen over an empty folder, with the given history file; typed text goes in through `key`.
fn screen(dir: &Path, history: Option<PathBuf>) -> Screen {
    let comp = Arc::new(Completions::new(dir.to_path_buf(), vec![], Box::new(Vec::new), Arc::new(Mutex::new(vec![]))));
    Screen::new(Tui::new(dir), Arc::new(String::new), Arc::new(String::new), comp, history)
}

fn type_text(s: &mut Screen, text: &str) -> Vec<String> {
    let mut submitted = vec![];
    for c in text.chars() {
        let k = if c == '\n' { key(KeyCode::Enter, KeyModifiers::NONE) } else { key(KeyCode::Char(c), KeyModifiers::NONE) };
        if let Action::Submit(t) = s.key(k) {
            submitted.push(t);
        }
    }
    submitted
}

#[test]
fn input_box_enter_submits_alt_enter_adds_a_line_and_history_walks_back() {
    let dir = tempfile::tempdir().unwrap();
    let hist = dir.path().join("h");
    // each "session" is a fresh input box over the same history file, like the Python helper
    let first = type_text(&mut screen(dir.path(), Some(hist.clone())), "first message\n");
    assert_eq!(first, vec!["first message"]);
    let mut s = screen(dir.path(), Some(hist.clone()));
    type_text(&mut s, "a");
    s.key(key(KeyCode::Enter, KeyModifiers::ALT));
    type_text(&mut s, "b");
    assert_eq!(s.editor.text(), "a\nb");
    assert!(matches!(s.key(key(KeyCode::Enter, KeyModifiers::NONE)), Action::Submit(t) if t == "a\nb"));
    let mut s = screen(dir.path(), Some(hist.clone()));
    s.key(key(KeyCode::Up, KeyModifiers::NONE)); // Up recalls the previous entry
    assert_eq!(s.editor.text(), "a\nb");
    let mut s = screen(dir.path(), None);
    type_text(&mut s, "/co");
    s.key(key(KeyCode::Tab, KeyModifiers::NONE)); // Tab highlights a command; Enter takes it, a second Enter submits
    assert!(matches!(s.key(key(KeyCode::Enter, KeyModifiers::NONE)), Action::None));
    match s.key(key(KeyCode::Enter, KeyModifiers::NONE)) {
        Action::Submit(t) => assert!(t.starts_with("/co"), "{t}"),
        _ => panic!("enter should submit"),
    }
}

#[test]
fn input_box_ctrl_c_clears_then_interrupts_and_ctrl_d_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = screen(dir.path(), None);
    type_text(&mut s, "oops");
    assert!(matches!(s.key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), Action::None)); // first Ctrl+C clears the line
    assert!(s.editor.is_empty());
    type_text(&mut s, "fresh");
    assert!(matches!(s.key(key(KeyCode::Enter, KeyModifiers::NONE)), Action::Submit(t) if t == "fresh"));
    assert!(matches!(s.key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), Action::Interrupt)); // on an empty line it interrupts
    assert!(matches!(s.key(key(KeyCode::Char('d'), KeyModifiers::CONTROL)), Action::Eof));
}

#[test]
fn editor_history_is_kept_between_boxes_and_repeats_are_not_stored() {
    let dir = tempfile::tempdir().unwrap();
    let hist = dir.path().join("hist");
    let mut e = Editor::new(Some(hist.clone()));
    for t in ["one", "one", "two"] {
        e.set(t);
        e.take();
    }
    assert_eq!(std::fs::read_to_string(&hist).unwrap().lines().collect::<Vec<_>>(), vec!["one", "two"]);
}

#[tokio::test]
async fn copy_skips_rail_and_border() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    sink::install();
    sink::write(" ◆ hello world\n ▎ second line\n│ boxed text   │\n");
    let mut s = screen(dir.path(), None);
    let copied = s.all_text();
    sink::uninstall();
    assert_eq!(copied, "hello world\nsecond line\nboxed text");
}
