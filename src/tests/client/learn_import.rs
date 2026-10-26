//! Ported from tests/test_learn_import.py.
use crate::agent::learn::Learned;
use crate::agent::learn_import::{chunks, collect, episodes, evidence, history_tasks, main, project_dirs, rulebook, story};
use crate::tests::fakes;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn rec(kind: &str, content: Value, extra: Value) -> String {
    let mut v = json!({"type": kind, "message": {"content": content}});
    if let Some(o) = extra.as_object() {
        for (k, x) in o {
            v[k] = x.clone();
        }
    }
    v.to_string()
}

fn user(text: &str) -> String {
    rec("user", json!(text), json!({}))
}

fn edit(path: &str) -> String {
    rec("assistant", json!([{"type": "tool_use", "name": "Edit", "input": {"file_path": path}}]), json!({}))
}

#[test]
fn rewards_files_and_hooks() {
    // main() resolves the repo path before collecting; on macOS the temp dir is a symlink (/var -> /private/var)
    let dir = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(dir.path()).unwrap().join("shop");
    std::fs::create_dir_all(&repo).unwrap();
    let projects = dir.path().join("projects");
    let key = repo.to_string_lossy().replace('/', "-");
    let folder = projects.join(&key);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(projects.join(format!("{key}x"))).unwrap(); // "shopx" is another repo, not a subfolder
    let lines = vec![
        user("add a refund button to the booking page"),
        edit(&repo.join("web/booking.tsx").to_string_lossy()),
        edit("/elsewhere/scratch.py"), // outside the repo: ignored
        user("no that's the wrong page, use the admin one"), // corrects the task before it
        edit(&repo.join("admin/booking.tsx").to_string_lossy()),
        rec("user", json!([{"type": "tool_result", "content": "PreToolUse:Bash hook error: lint first", "tool_use_id": "t"}]), json!({"toolDenialKind": "permission-rule"})),
        user("commit it in parts pls"), // approves the admin task
        edit(&repo.join("x.py").to_string_lossy()),
        rec("user", json!([{"type": "text", "text": "[Request interrupted by user]"}]), json!({})), // punishes the commit task
        user("<command-name>/clear</command-name>"), // a slash command, not a task
        rec("user", json!("subagent noise"), json!({"isSidechain": true})),
    ];
    std::fs::write(folder.join("s.jsonl"), lines.join("\n")).unwrap();

    assert_eq!(project_dirs(&repo, &projects), vec![folder.clone()]);
    let (tasks, hooks, seen) = collect(&repo, &projects);
    assert_eq!(seen, ["s"]);
    assert_eq!(tasks.iter().map(|t| t.reward).collect::<Vec<_>>(), [-1, 1, -1]);
    assert_eq!(tasks[0].files, ["web/booking.tsx"]);
    assert_eq!(tasks[1].files, ["admin/booking.tsx"]);
    assert_eq!(hooks, BTreeMap::from([("lint first".to_string(), 1)]));

    let text = evidence(&tasks, &hooks, &["use pnpm".to_string()]);
    assert!(text.contains("(1x) lint first") && text.contains("Already known:\n- use pnpm"), "{text}");
    assert!(text.contains("Replies right after the agent got it wrong:\n- no that's the wrong page"), "{text}");
}

#[test]
fn deleted_sessions_come_back_from_prompt_history() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("shop");
    let history = dir.path().join("history.jsonl");
    let repo_s = repo.to_string_lossy().to_string();
    let rows = vec![
        json!({"display": "add coupons to checkout", "timestamp": 1, "project": repo_s, "sessionId": "old"}),
        json!({"display": "no, coupons go on the cart page", "timestamp": 2, "project": repo.join("apps/web").to_string_lossy(), "sessionId": "old"}),
        json!({"display": "/clear", "timestamp": 3, "project": repo_s, "sessionId": "old"}), // a slash command is not a task
        json!({"display": "kept transcript", "timestamp": 4, "project": repo_s, "sessionId": "s"}), // transcript still on disk
        json!({"display": "other repo", "timestamp": 5, "project": format!("{repo_s}x"), "sessionId": "x"}),
    ];
    std::fs::write(&history, rows.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
    let tasks = history_tasks(&repo, &history, &["s".to_string()]);
    let got: Vec<(&str, i32)> = tasks.iter().map(|t| (t.request.as_str(), t.reward)).collect();
    assert_eq!(got, [("add coupons to checkout", -1), ("no, coupons go on the cart page", 0)]);
    // a punished task shows what you said next
    assert!(story(&tasks[0]).contains("YOU NEXT: no, coupons go on the cart page"));
}

#[test]
fn story_names_failed_commands_and_chunks_split() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("shop");
    let lines = vec![
        user("run the server tests"),
        rec("assistant", json!([{"type": "tool_use", "id": "b1", "name": "Bash", "input": {"command": "npx vitest run"}}]), json!({})),
        rec("user", json!([{"type": "tool_result", "tool_use_id": "b1", "is_error": true, "content": "vitest: command not found"}]), json!({})),
        rec("assistant", json!([{"type": "text", "text": "vitest is missing, switching to pnpm test"}]), json!({})),
        user("never use vitest here, it's playwright only"),
    ];
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (tasks, _) = episodes(&refs, &repo);
    let s = story(&tasks[0]);
    assert!(s.contains("failed: npx vitest run -> vitest: command not found") && s.contains("CLAUDE: vitest is missing"), "{s}");
    let many: Vec<_> = (0..50).flat_map(|_| tasks.clone()).collect();
    let parts = chunks(&many, 500);
    assert!(parts.len() > 1 && parts.iter().all(|c| c.len() <= 1000));
}

#[test]
fn rulebook_skips_mews_own_section() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("apps/web")).unwrap();
    std::fs::write(d.join("AGENTS.md"), "# Rules\nNever read .env\n\n## Learned by melon\n- old lesson under review\n").unwrap();
    std::fs::write(d.join("apps/web/AGENTS.md"), "Ask before UI changes").unwrap();
    std::fs::create_dir_all(d.join(".claude/skills/debug")).unwrap();
    std::fs::write(d.join(".claude/skills/debug/SKILL.md"), "---\nname: debug\ndescription: Use for any 500\n---\n").unwrap();
    let book = rulebook(d);
    assert!(book.contains("Never read .env") && book.contains("Ask before UI changes") && book.contains("Use for any 500"), "{book}");
    assert!(!book.contains("old lesson under review"));
}

#[tokio::test]
async fn apply_writes_the_reviewed_dry_run_without_a_model() {
    // Python swapped Learned for one with a private base folder. Here the suite's data folder is already private
    // (fakes::init), and the repo path is unique to this test, so its lessons file is too.
    fakes::init();
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("shop");
    std::fs::create_dir_all(&repo).unwrap();
    let mut l = Learned::new(&repo);
    l.add_lesson("this lesson is noise and goes", "user");
    l.add_lesson("this lesson is fine and stays", "user");
    let saved = l.store_path().with_extension("deep.json");
    std::fs::write(
        &saved,
        json!({"drop": ["this lesson is noise and goes"], "add": ["Quote bracketed Next.js paths in shell commands"], "why": {}, "candidates": []}).to_string(),
    )
    .unwrap();
    // with a saved dry run, --apply must not ask a model for anything (a model call here would fail: no key, no network)
    let none = dir.path().join("none").to_string_lossy().to_string();
    let argv = ["learn_import", "--repo", &repo.to_string_lossy(), "--projects", &none, "--history", &format!("{none}.jsonl"), "--deep", "--apply"];
    let code = main(argv.iter().map(|s| s.to_string()).collect()).await;
    assert_eq!(code, 0);
    assert_eq!(Learned::new(&repo).lessons(), ["this lesson is fine and stays", "Quote bracketed Next.js paths in shell commands"]);
    let agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert!(!agents.contains("noise") && agents.contains("- Quote bracketed Next.js paths"), "{agents}");
    assert!(!saved.exists()); // used up, so the next run starts fresh
}
