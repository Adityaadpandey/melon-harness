//! Ported from tests/test_diagnostics.py: a change that breaks a file is reported in the edit's own result, and only
//! what the change broke.
//!
//! Not ported: test_a_project_still_loading_skips_one_check_and_answers_the_next. It shrinks the Python TS_TIMEOUT
//! module variable with monkeypatch; the Rust timeout is a private const, so there is nothing to turn down.
use crate::tests::fakes::{answer, call, config, run, scripted, text};
use crate::util::diagnostics::new_problems;
use serde_json::json;
use std::path::{Path, PathBuf};

async fn found(name: &str, old: &str, new: &str) -> Vec<String> {
    new_problems(Path::new(name), Some(old), new, Path::new(".")).await
}

#[tokio::test]
async fn a_new_syntax_error_is_reported_but_an_old_one_is_not() {
    assert!(found("a.py", "x = 1\n", "x = (1\n").await[0].contains("SyntaxError"));
    let broken = "def f(:\n    pass\n";
    // it was already broken; not this edit's doing
    assert!(found("a.py", broken, &format!("{broken}y = 2\n")).await.is_empty());
}

#[tokio::test]
async fn data_files_are_parsed() {
    assert!(!found("c.json", r#"{"a": 1}"#, r#"{"a": 1,}"#).await.is_empty());
    assert!(!found("c.toml", "a = 1\n", "a = \n").await.is_empty());
    assert!(found("c.toml", "a = 1\n", "a = 2\n").await.is_empty());
}

#[tokio::test]
async fn javascript_syntax_errors_come_from_node() {
    if which::which("node").is_err() {
        return; // node not installed
    }
    assert!(!found("x.mjs", "export const a = 1\n", "export const a = (1\n").await.is_empty());
    // module syntax is fine
    assert!(found("x.js", "import x from 'y'\n", "import x from 'y'\nx()\n").await.is_empty());
}

#[tokio::test]
async fn a_broken_edit_says_so_in_its_result() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("m.py"), "def f():\n    return 1\n").unwrap();
    let mut cfg = config(d.path());
    cfg.approval = crate::config::ApprovalPolicy::Auto;
    let mut agent = scripted(
        cfg,
        vec![
            call("read_file", json!({"path": "m.py"})),
            call("edit", json!({"path": "m.py", "old_string": "return 1", "new_string": "return (1"})),
            text("done"),
        ],
    )
    .await;
    let events = run(&mut agent, "change it").await;
    assert!(answer(&events).is_some());
    let requests = agent.session.client.requests.lock().unwrap();
    let last = requests.last().unwrap();
    let results: Vec<String> = last.iter().filter(|m| m["role"] == "tool").map(|m| m["content"].to_string()).collect();
    assert!(results.iter().any(|r| r.contains("<diagnostics") && r.contains("SyntaxError")), "{results:?}");
}

/// Any TypeScript on this machine to borrow (tests never install one).
fn typescript_install() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let roots = [home.join("Devlopment/tunyt"), PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.join("pi")];
    roots.into_iter().map(|r| r.join("node_modules/typescript")).find(|t| t.join("lib/tsserver.js").is_file())
}

#[cfg(unix)]
#[tokio::test]
async fn typescript_errors_come_from_the_projects_own_tsserver() {
    let Some(ts) = typescript_install().filter(|_| which::which("node").is_ok()) else { return }; // no TypeScript install to borrow
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    std::fs::create_dir(p.join("node_modules")).unwrap();
    std::os::unix::fs::symlink(&ts, p.join("node_modules/typescript")).unwrap();
    std::fs::write(p.join("tsconfig.json"), r#"{"compilerOptions": {"strict": true, "noEmit": true}}"#).unwrap();
    let src = p.join("src/a.ts");
    std::fs::create_dir(p.join("src")).unwrap();
    let old = "export function total(xs: number[]): number {\n  return xs.reduce((a, b) => a + b, 0)\n}\n";
    let new = old.replace("return xs", "const label: string = xs.length\n  return xs");
    std::fs::write(&src, &new).unwrap();
    let broke = new_problems(&src, Some(old), &new, p).await;
    // the error was already there: not this edit's
    let same = new_problems(&src, Some(&new), &format!("{new}\n"), p).await;
    assert!(broke.len() == 1 && broke[0].contains("TS2322") && broke[0].starts_with("2:"), "{broke:?}");
    assert!(same.is_empty(), "{same:?}");
}
