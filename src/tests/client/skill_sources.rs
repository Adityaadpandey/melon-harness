//! Ported from tests/test_skill_sources.py.
//! Python gave each test its own fake home through monkeypatching. Here HOME is process-wide and shared with the other
//! tests of this group, so one fake home is built once (see trust::isolated_home) and every test uses that one.
use super::trust::isolated_home;
use crate::agent::skills::{load_skills, match_skills};
use crate::config::{Config, ConfigHandle};
use crate::prompts::get_system_prompt;
use crate::safety::guard::violation;
use crate::tests::fakes;
use crate::tools::registry::create_default_registry;
use serde_json::json;

fn repo_config() -> (Config, std::path::PathBuf) {
    fakes::init();
    let home = isolated_home();
    let mut c = Config::default();
    c.cwd = home.join("repo");
    (c, home)
}

#[test]
fn claude_code_skills_load_without_hijacking_requests_or_mews_own_ponytail() {
    let (c, _) = repo_config();
    // the dead link is skipped, not a crash
    let skills = load_skills(&c);
    let get = |n: &str| skills.iter().find(|s| s.name == n).unwrap_or_else(|| panic!("no skill {n}")).clone();
    assert_eq!(get("tidy").description, "Keeps diffs small and readable");
    assert_eq!(get("review").description, "Review a diff");
    assert!(get("ponytail").always && !get("ponytail").description.contains("Claude Code")); // melon's compact one wins
    let matched: Vec<String> = match_skills(&skills, "review this diff", 4).into_iter().map(|s| s.name).collect();
    assert!(!matched.contains(&"review".to_string())); // listed, never auto-fired
}

#[test]
fn pi_prompt_inlines_always_on_skills_and_lists_the_rest_once_per_folder() {
    let (c, _) = repo_config();
    let reg = create_default_registry(ConfigHandle::new(c.clone()));
    let prompt = get_system_prompt(&c, None, &reg.get_tools(), &load_skills(&c));
    assert!(prompt.contains("the least that fully works")); // ponytail applies on every request, no read needed
    // machine-wide skills say when to use them too
    assert!(prompt.contains("~/.claude/skills/\n- review/SKILL.md: Review a diff"), "{prompt}");
    assert_eq!(prompt.matches("/acme/1.0/skills/").count(), 1);
    assert!(prompt.contains("- tidy/SKILL.md"));
}

#[test]
fn skill_files_are_readable_from_any_repo_but_nothing_else_outside_it() {
    let (c, home) = repo_config();
    assert!(violation("read_file", &json!({"path": "~/.claude/skills/review/SKILL.md"}), &c).is_none());
    assert!(violation("read_file", &json!({"path": home.join(".claude").join("settings.json").to_string_lossy()}), &c).is_some());
    assert!(violation("write_file", &json!({"path": "~/.claude/skills/review/SKILL.md", "content": "x"}), &c).is_some());
}
