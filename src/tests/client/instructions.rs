//! Ported from tests/test_instructions.py.
use crate::agent::instructions::for_path;
use crate::config::loader::instructions_for;
use std::collections::HashSet;

#[test]
fn nested_instructions_show_once_outermost_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("AGENTS.md"), "Use pnpm.").unwrap();
    let web = root.join("apps").join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("CLAUDE.md"), "Run check-types.\n@docs.md").unwrap();
    std::fs::write(web.join("docs.md"), "Pages live in (list).").unwrap();
    let mut seen = HashSet::new();
    let out = for_path(root, &web.join("src").join("a.ts"), &mut seen);
    assert!(out.find("Use pnpm.").unwrap() < out.find("Run check-types.").unwrap());
    assert!(out.contains("Pages live in (list)."));
    assert_eq!(for_path(root, &web.join("src").join("b.ts"), &mut seen), ""); // already shown
}

#[test]
fn import_outside_the_repo_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(dir.path().join("secret.md"), "TOP SECRET").unwrap();
    std::fs::write(repo.join("AGENTS.md"), "@../secret.md").unwrap();
    assert!(!for_path(&repo, &repo.join("a.py"), &mut HashSet::new()).contains("TOP SECRET"));
}

#[test]
fn file_outside_root_gets_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "x").unwrap();
    assert_eq!(for_path(&dir.path().join("sub"), &dir.path().join("other.py"), &mut HashSet::new()), "");
}

#[test]
fn taste_goes_into_the_prompt_and_nested_taste_shows_on_touch() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home) = (dir.path().join("repo"), dir.path().join("home"));
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join("AGENTS.md"), "Use pnpm for every script in this repo, never npm.").unwrap();
    let taste = repo.join(".commandcode").join("taste");
    std::fs::create_dir_all(taste.join("ui")).unwrap();
    std::fs::write(
        taste.join("taste.md"),
        "# Taste (Continuously Learned by [CommandCode][cmd])\n\n[cmd]: https://commandcode.ai/\n\n# UI\nSee [ui/taste.md]\n\n# Money\n- Money is integer paise. Confidence: 0.75\n",
    )
    .unwrap();
    std::fs::write(taste.join("ui").join("taste.md"), "# UI\n- Never use emojis in UI. Confidence: 0.85\n").unwrap();
    let web = repo.join("apps").join("web").join(".commandcode").join("taste");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("taste.md"), "- Use a peel effect for loading screens. Confidence: 0.70\n").unwrap();

    let out = instructions_for(&repo, Some(&home)).unwrap();
    let at = |s: &str| out.find(s).unwrap_or_else(|| panic!("missing {s:?} in {out}"));
    assert!(at("Use pnpm") < at("Learned taste") && at("Learned taste") < at("integer paise") && at("integer paise") < at("Never use emojis"));
    assert!(!out.contains("Continuously Learned") && !out.contains("[cmd]") && !out.contains("See [ui"));
    assert!(!out.contains("peel effect")); // a subfolder's taste waits until that folder is touched

    let mut seen = HashSet::new();
    let shown = for_path(&repo, &repo.join("apps").join("web").join("src").join("a.tsx"), &mut seen);
    assert!(shown.contains("peel effect") && !shown.contains("integer paise")); // the root's is already in the prompt
}
