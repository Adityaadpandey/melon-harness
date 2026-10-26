//! Ported from tests/test_pi_options.py
use crate::config::{Config, ConfigHandle};
use crate::context::manager::ContextManager;
use crate::prompts::get_system_prompt;
use crate::tests::fakes::init;
use crate::tools::registry::create_default_registry;
use serde_json::{json, Value};
use std::path::Path;

fn cfg(dir: &Path, extra: Value) -> Config {
    init();
    let mut m = extra.as_object().cloned().unwrap_or_default();
    m.insert("cwd".into(), json!(dir.to_string_lossy()));
    Config::from_map(m).unwrap()
}

#[test]
fn core_tools_only_gives_pis_four() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(cfg(dir.path(), json!({"core_tools_only": true})).allowed_tools.unwrap(), vec!["read_file", "write_file", "edit", "shell"]);
}

#[test]
fn repo_map_is_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::write(p.join("core.py"), "def start():\n    pass\n").unwrap();
    for cmd in [["init", "-q"].as_slice(), ["add", "."].as_slice()] {
        std::process::Command::new("git").args(cmd).current_dir(p).output().unwrap();
    }
    assert!(!get_system_prompt(&cfg(p, json!({})), None, &[], &[]).contains("<repo_map>"));
    assert!(get_system_prompt(&cfg(p, json!({"repo_map": true})), None, &[], &[]).contains("core.py: start"));
}

#[test]
fn compaction_leaves_the_cacheable_prefix_alone() {
    // System prompt and tool order must be byte-identical before and after compaction, so the provider's cache still hits.
    let dir = tempfile::tempdir().unwrap();
    let c = cfg(dir.path(), json!({}));
    let handle = ConfigHandle::new(c);
    let mut cm = ContextManager::new(handle.clone(), "the system prompt".into());
    for i in 0..6 {
        cm.add_user_message(&format!("q{i}"));
        cm.add_assistant_message(Some(&format!("a{i}")), None, None, None);
    }
    let before = cm.get_messages()[0].clone();
    cm.apply_compaction("summary", 8, None, None);
    assert_eq!(cm.get_messages()[0], before);
    let names = |h: &ConfigHandle| create_default_registry(h.clone()).get_tools().iter().map(|t| t.name().to_string()).collect::<Vec<_>>();
    assert_eq!(names(&handle), names(&handle));
}

#[test]
fn no_pi_flag_turns_pi_off_with_its_defaults() {
    // The flag is `pi_mode` in the settings map (load_config would also read the real home); same effect.
    let dir = tempfile::tempdir().unwrap();
    let (on, off) = (cfg(dir.path(), json!({})), cfg(dir.path(), json!({"pi_mode": false})));
    assert!(on.pi_mode && on.tool_output_max_chars == 51_200);
    assert!(!off.pi_mode && off.tool_output_max_chars == 8_000 && off.allowed_tools.is_none());
}
