//! Ported from tests/test_notes.py: task notes round-trip through disk, show up in the prompt, go stale, and the
//! notes tool exposes write/read/clear. The data folder is the shared temp one from fakes::init, and notes are keyed
//! by workspace, so each test's own temp dir keeps them apart.
use super::{handle, invocation};
use crate::agent::notes;
use crate::tools::builtin::notes::NotesTool;
use crate::tools::Tool;
use serde_json::{json, Value};

#[test]
fn notes_roundtrip_and_prompt() {
    let d = tempfile::tempdir().unwrap();
    let work = d.path().join("repo");
    std::fs::create_dir(&work).unwrap();
    crate::tests::fakes::init();
    notes::write(&work, "Goal: move pages\nNext: run check-types");
    assert!(notes::read(&work).contains("check-types"));
    let section = notes::prompt_section(&work).unwrap();
    assert!(section.contains("Task notes") && section.contains("Goal: move pages"));
    notes::write(&work, "");
    assert_eq!(notes::read(&work), "");
    assert!(notes::prompt_section(&work).is_none());
}

#[test]
fn stale_notes_are_not_shown() {
    let d = tempfile::tempdir().unwrap();
    crate::tests::fakes::init();
    notes::write(d.path(), "old plan");
    // Python patches age_days to 30; here the file itself is made 30 days old
    let thirty_days = std::time::Duration::from_secs(30 * 86400);
    let f = std::fs::OpenOptions::new().write(true).open(notes::path_for(d.path())).unwrap();
    f.set_modified(std::time::SystemTime::now() - thirty_days).unwrap();
    assert!(notes::prompt_section(d.path()).is_none());
}

#[tokio::test]
async fn tool_actions() {
    let d = tempfile::tempdir().unwrap();
    let tool = NotesTool::new(handle(d.path()));
    let call = |p: Value| {
        let inv = invocation(d.path(), p);
        let tool = &tool;
        async move { tool.execute(&inv).await }
    };
    assert!(call(json!({"action": "write", "text": "Done: a"})).await.success);
    assert!(call(json!({"action": "read"})).await.output.contains("Done: a"));
    assert!(!call(json!({"action": "write"})).await.success);
    call(json!({"action": "clear"})).await;
    assert!(call(json!({"action": "read"})).await.output.contains("no notes"));
}
