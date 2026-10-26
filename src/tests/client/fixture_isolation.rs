//! Ported from tests/test_fixture_isolation.py.
//! Three small repos with different stacks, used to prove one project's facts, memory and files never leak into
//! another's session.
use crate::agent::learn::Learned;
use crate::config::{ApprovalPolicy, Config, ConfigHandle};
use crate::hooks::HookSystem;
use crate::prompts::get_system_prompt;
use crate::tests::fakes;
use crate::tools::registry::create_default_registry;
use std::collections::BTreeMap;
use std::path::PathBuf;

const STACKS: &[(&str, &[(&str, &str)])] = &[
    ("webapp", &[("package.json", "{\"scripts\": {\"test\": \"vitest\"}}"), ("src/App.tsx", "export {}")]),
    ("service", &[("pyproject.toml", "[tool.pytest]"), ("app/main.py", "print(1)")]),
    ("gocli", &[("go.mod", "module gocli"), ("main.go", "package main")]),
];

fn repos(base: &std::path::Path) -> BTreeMap<&'static str, PathBuf> {
    let mut out = BTreeMap::new();
    for (name, files) in STACKS {
        let root = base.join(name);
        for (rel, body) in *files {
            std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
            std::fs::write(root.join(rel), body).unwrap();
        }
        out.insert(*name, root);
    }
    out
}

fn config_in(root: &std::path::Path) -> Config {
    fakes::init();
    let mut c = Config::default();
    c.cwd = root.to_path_buf();
    c
}

#[test]
fn learned_lessons_stay_in_their_repo() {
    fakes::init();
    let dir = tempfile::tempdir().unwrap();
    let r = repos(dir.path());
    let base = dir.path().join("data");
    Learned::with_base(&r["webapp"], &base).add_lesson("always run vitest before finishing a change", "user");
    for other in ["service", "gocli"] {
        let agents = r[other].join("AGENTS.md");
        assert!(!agents.is_file() || !std::fs::read_to_string(agents).unwrap().contains("vitest"));
    }
    assert!(std::fs::read_to_string(r["webapp"].join("AGENTS.md")).unwrap().contains("vitest"));
}

#[test]
fn prompt_names_only_the_active_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let r = repos(dir.path());
    for (name, root) in &r {
        let c = config_in(root);
        let reg = create_default_registry(ConfigHandle::new(c.clone()));
        let prompt = get_system_prompt(&c, None, &reg.get_tools(), &[]);
        assert!(prompt.contains(&root.to_string_lossy().to_string()));
        for (n, other) in &r {
            if n != name {
                assert!(!prompt.contains(&other.to_string_lossy().to_string()), "{name} prompt names {n}");
            }
        }
    }
}

#[tokio::test]
async fn tools_cannot_write_into_a_sibling_repo() {
    let dir = tempfile::tempdir().unwrap();
    let r = repos(dir.path());
    let mut c = config_in(&r["webapp"]);
    c.approval = ApprovalPolicy::Yolo;
    let reg = create_default_registry(ConfigHandle::new(c.clone()));
    let target = r["service"].join("app").join("main.py");
    let params = serde_json::json!({"path": target.to_string_lossy(), "content": "pwned"});
    let res = reg.invoke("write_file", params, &r["webapp"], &HookSystem::new(&c), None, "c1").await;
    assert!(!res.success);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "print(1)");
}
