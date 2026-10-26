//! Ported from tests/test_commands.py: Claude-style command files and slash completion.
use crate::ui::input::Completions;
use crate::util::templates::{expand, load_commands, split_args};
use std::sync::{Arc, Mutex};

#[test]
fn claude_style_command_files() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join(".claude/commands");
    std::fs::create_dir_all(d.join("review")).unwrap();
    std::fs::write(d.join("fix.md"), "---\ndescription: Fix an issue\n---\nFix issue $1 and add a test. Notes: $ARGUMENTS").unwrap();
    std::fs::write(d.join("review/security.md"), "Review $ARGUMENTS for security problems").unwrap();
    let found = load_commands(&[d]);
    assert_eq!(found["fix"].1, "Fix an issue");
    assert!(found["review:security"].1.starts_with("Review"));
    assert_eq!(expand(&found["fix"].0, &split_args("42 quickly")), "Fix issue 42 and add a test. Notes: 42 quickly");
}

#[test]
fn slash_completion_lists_skills() {
    let tmp = tempfile::tempdir().unwrap();
    let skills = Arc::new(Mutex::new(vec![("code-review".to_string(), "review a diff".to_string())]));
    let comp = Completions::new(tmp.path().to_path_buf(), vec![], Box::new(Vec::new), skills);
    let (_, pairs) = comp.complete("/code", 5).unwrap();
    assert!(pairs.iter().any(|p| p.replacement == "/code-review"), "{:?}", pairs.iter().map(|p| &p.replacement).collect::<Vec<_>>());
}
