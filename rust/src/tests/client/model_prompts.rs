//! Ported from tests/test_model_prompts.py.
//! The prompt fits the model it is sent to: open-model notes where they help, a skill list sized to the window.
use crate::agent::skills::Skill;
use crate::config::{Config, ConfigHandle};
use crate::prompts::{get_system_prompt, model_notes};
use crate::tests::fakes;
use crate::tools::registry::create_default_registry;
use std::path::PathBuf;

fn prompt(model: &str, window: usize, skills: &[Skill], pi: bool) -> String {
    fakes::init();
    let mut cfg = Config::default();
    cfg.cwd = std::env::temp_dir();
    cfg.pi_mode = pi;
    cfg.model.name = model.into();
    cfg.model.context_window = window;
    let reg = create_default_registry(ConfigHandle::new(cfg.clone()));
    get_system_prompt(&cfg, None, &reg.get_tools(), skills)
}

fn p(model: &str) -> String {
    prompt(model, 256_000, &[], true)
}

#[test]
fn open_models_get_the_notes_tuned_ones_do_not() {
    assert!(p("deepseek/deepseek-v4-pro").contains("written as text") && p("openai/gpt-oss-120b").contains("written as text"));
    assert!(!p("claude-sonnet-5-5").contains("written as text") && !p("codex/gpt-5.6-luna").contains("written as text"));
    assert!(p("moonshotai/Kimi-K3").contains("Make minimal changes"));
    assert!(prompt("Qwen/Qwen3.8-Max", 256_000, &[], false).contains("written as text")); // the guarded prompt too
    assert!(model_notes("claude-opus-5-5").is_empty());
}

#[test]
fn the_skill_list_shrinks_to_fit_a_small_window() {
    let skills: Vec<Skill> = (0..100)
        .map(|i| Skill {
            name: format!("skill-{i}"),
            description: format!("Use when the user asks about topic number {i} and wants it done carefully."),
            path: Some(PathBuf::from(format!("/x/skills/skill-{i}/SKILL.md"))),
            ..Default::default()
        })
        .collect();
    let (big, small) = (prompt("m", 1_000_000, &skills, true), prompt("m", 32_000, &skills, true));
    // names stay, details go
    assert!(small.contains("skill-99") && big.contains("topic number 99") && !small.contains("topic number 99"));
    assert!(small.len() + 5000 < big.len());
}
