//! Ported from Python tests (see each file's header).
use crate::config::Config;
use serde_json::{json, Value};
use std::path::Path;

mod test_agent_files;
mod test_answer_cap;
mod test_ask_user;
mod test_background_shell;
mod test_challenge;
mod test_commands;
mod test_precompact;

/// A config rooted in `dir` that approves everything, built the way the loader builds it (so pi-mode defaults apply,
/// like Python's `Config(cwd=..., approval=AUTO)`). `extra` overrides or adds keys, e.g. `json!({"pi_mode": false})`.
pub(crate) fn cfg(dir: &Path, extra: Value) -> Config {
    super::fakes::init();
    let mut map = serde_json::Map::new();
    map.insert("cwd".into(), json!(dir));
    map.insert("approval".into(), json!("auto"));
    if let Some(o) = extra.as_object() {
        for (k, v) in o {
            map.insert(k.clone(), v.clone());
        }
    }
    Config::from_map(map).expect("test config")
}

/// `git init` in `dir` with a committer, so "which files changed" works.
pub(crate) fn git_init(dir: &Path) {
    for a in [&["init", "-q"][..], &["config", "user.email", "a@b"], &["config", "user.name", "t"]] {
        let ok = std::process::Command::new("git").args(a).current_dir(dir).output().unwrap().status.success();
        assert!(ok, "git {a:?} failed");
    }
}
