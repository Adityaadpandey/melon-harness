//! Ported from tests/test_grounding.py.
use crate::agent::events::AgentEvent;
use crate::agent::grounding::{extract_refs, has_file_code, shows_second_thoughts, verify_references, wants_change};
use crate::config::{ApprovalPolicy, Config};
use crate::tests::fakes::*;
use serde_json::json;
use std::path::{Path, PathBuf};

fn git(p: &Path, args: &[&str]) {
    assert!(std::process::Command::new("git").args(args).current_dir(p).output().unwrap().status.success());
}

/// A git repo with apps/server/src/app.ts (50 lines) and a README.
fn repo(base: &Path) -> PathBuf {
    let root = base.join("repo");
    std::fs::create_dir_all(root.join("apps/server/src")).unwrap();
    git(&root, &["init", "-q"]);
    let lines: Vec<String> = (1..=50).map(|i| format!("line {i}")).collect();
    std::fs::write(root.join("apps/server/src/app.ts"), lines.join("\n")).unwrap();
    std::fs::write(root.join("README.md"), "hi\n").unwrap();
    root
}

fn cfg(root: &Path) -> Config {
    let mut c = config(root);
    c.pi_mode = false;
    c.approval = ApprovalPolicy::Auto;
    c.auto_verify = false;
    c.auto_orient = false;
    c
}

#[tokio::test]
async fn references_resolve_when_started_in_a_folder_that_holds_the_repo() {
    let dir = tempfile::tempdir().unwrap();
    repo(dir.path()); // the repo sits one level below the working directory, which is not a repo
    let (n, problems) = verify_references("See apps/server/src/app.ts:12 and src/ghost.ts:3.", dir.path()).await;
    assert_eq!(n, 2);
    assert_eq!(problems.iter().map(|p| p.ref_.as_str()).collect::<Vec<_>>(), ["src/ghost.ts:3"]);
}

#[test]
fn extracts_paths_and_lines_but_not_urls_versions_or_bare_names() {
    let refs = extract_refs("See `src/app.ts:12-30`, README.md:1, https://example.com/a/b.ts:9, v1.2.3, and index.ts alone, plus lib/x.py");
    assert!(refs.contains(&("src/app.ts".to_string(), Some(12), Some(30))));
    assert!(refs.contains(&("README.md".to_string(), Some(1), None)));
    assert!(refs.contains(&("lib/x.py".to_string(), None, None)));
    assert!(!refs.iter().any(|r| r.0.contains("example") || r.0 == "index.ts"), "{refs:?}");
}

#[tokio::test]
async fn verifies_existing_missing_and_out_of_range_references() {
    let dir = tempfile::tempdir().unwrap();
    let root = repo(dir.path());
    let (n, problems) = verify_references(
        "Fixed in apps/server/src/app.ts:12 and src/app.ts:49, but see src/ghost.ts:3 and apps/server/src/app.ts:999 and README.md.",
        &root,
    )
    .await;
    // a bare `README.md` is too ambiguous to judge, so it is skipped
    assert_eq!(n, 4);
    // sub-app relative citations resolve; fabrications do not
    let bad: std::collections::BTreeMap<&str, &str> = problems.iter().map(|p| (p.ref_.as_str(), p.why.as_str())).collect();
    assert_eq!(bad.keys().copied().collect::<Vec<_>>(), ["apps/server/src/app.ts:999", "src/ghost.ts:3"]);
    assert!(bad["apps/server/src/app.ts:999"].contains("50 lines"));
}

fn end_report(evs: &[AgentEvent]) -> serde_json::Value {
    evs.iter().find_map(|e| if let AgentEvent::AgentEnd { report, .. } = e { Some(report.clone()) } else { None }).expect("agent end")
}

#[tokio::test]
async fn fabricated_reference_is_sent_back_once_then_reported() {
    let dir = tempfile::tempdir().unwrap();
    let root = repo(dir.path());
    let mut a = scripted(cfg(&root), vec![text("The bug is in src/invented.ts:7."), text("It is in apps/server/src/app.ts:12.")]).await;
    let evs = run(&mut a, "where is the bug?").await;
    let rep = end_report(&evs);
    assert_eq!(rep["grounding"]["unverified"], json!([]));
    assert_eq!(rep["grounding"]["checked"], 1);
    let second = a.session.client.requests.lock().unwrap()[1].clone();
    assert!(second.last().unwrap()["content"].as_str().unwrap().contains("do not exist"));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice(m) if m.contains("could not be verified"))));
}

#[tokio::test]
async fn uncorrected_fabrication_stays_in_the_report_not_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let root = repo(dir.path());
    let mut a = scripted(cfg(&root), vec![text("See src/invented.ts:7."), text("Still src/invented.ts:7.")]).await;
    let evs = run(&mut a, "q").await;
    let rep = end_report(&evs);
    let un = rep["grounding"]["unverified"].as_array().unwrap();
    assert_eq!(un.len(), 1);
    assert!(un[0].as_str().unwrap().contains("invented.ts"));
}

#[tokio::test]
async fn edit_requires_a_prior_read_and_detects_changes_since() {
    let dir = tempfile::tempdir().unwrap();
    let root = repo(dir.path());
    let f = root.join("a.py");
    std::fs::write(&f, "x = 1\n").unwrap();
    let a = scripted(cfg(&root), vec![]).await;
    let (reg, hook, mgr) = (a.session.tool_registry.clone(), &a.session.hook_system, &a.session.approval_manager);
    let edit = |old: &'static str, new: &'static str| {
        let (reg, root) = (reg.clone(), root.clone());
        async move { reg.invoke("edit", json!({"path": "a.py", "old_string": old, "new_string": new}), &root, hook, Some(mgr), "e").await }
    };
    let r = edit("x = 1", "x = 2").await;
    assert!(!r.success && r.error.unwrap().contains("Read a.py with read_file"));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "x = 1\n");
    reg.invoke("read_file", json!({"path": "a.py"}), &root, hook, None, "r1").await;
    std::fs::write(&f, "x = 1  # edited by the user meanwhile\n").unwrap();
    let r = edit("x = 1", "x = 2").await;
    assert!(!r.success && r.error.unwrap().contains("changed on disk"));
    reg.invoke("read_file", json!({"path": "a.py"}), &root, hook, None, "r2").await;
    assert!(edit("x = 1", "x = 2").await.success);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "x = 2  # edited by the user meanwhile\n");
    // our own write counts as 'known content'
    assert!(edit("x = 2", "x = 3").await.success);
    // creating a file needs no read
    let w = reg.invoke("write_file", json!({"path": "brand_new.py", "content": "1"}), &root, hook, Some(mgr), "w").await;
    assert!(w.success);
}

#[test]
fn nudge_only_for_change_requests_with_source_code_in_the_reply() {
    assert!(wants_change("fix the pagination bug") && wants_change("add a remove command to the CLI"));
    assert!(!wants_change("Read-only. Open .env and tell me DATABASE_URL"));
    assert!(!wants_change("explain how check-in works") && !wants_change("where is the discount validated?"));
    assert!(has_file_code("Here:\n```python\nx = 1\n```\n") && has_file_code("```ts\nconst a = 1\n```"));
    assert!(!has_file_code("Run this:\n```bash\npsql -c 'select 1'\n```\n") && !has_file_code("```sql\nSELECT 1;\n```"));
    assert!(!has_file_code("no code here"));
}

#[test]
fn a_review_that_dismisses_its_own_findings_is_flagged_but_a_clean_one_is_not() {
    let messy = "high a.ts:1 keys clash ... actually a non-issue. Skip.\nmedium b.ts:2 extra insert. Acceptable; not a bug.";
    assert!(shows_second_thoughts("review my changes", messy));
    assert!(!shows_second_thoughts("explain the diff", messy)); // only reviews are judged
    assert!(!shows_second_thoughts("review my changes", "high a.ts:1 createMany failure 500s the page -> catch and fall back"));
}

#[test]
fn single_dismissals_and_a_final_answer_header_are_caught_but_no_issues_found_is_not() {
    assert!(shows_second_thoughts("review it", "HIGH a.ts:1 reads a cached ref. Consistent. No defect."));
    assert!(shows_second_thoughts("review it", "MEDIUM a.ts:9 two entries, coincidence works, not a real defect"));
    assert!(shows_second_thoughts("review it", "Final answer (verified only):\nHIGH a.ts:1 real thing -> fix"));
    assert!(!shows_second_thoughts("review it", "No issues found. Not run: tsc."));
}

#[test]
fn a_finding_that_runs_on_is_flagged_but_two_tight_lines_are_not() {
    let tight = "HIGH a.ts:1-9  a new insert on the request path 500s the page when it throws -> catch and serve without it\nLOW b.ts:4  stale prop type -> update it";
    let rambling = format!("MEDIUM a.ts:9 {}", "this changes how the value flows through the component and then the effect runs again after the render ".repeat(6));
    assert!(!shows_second_thoughts("review it", tight));
    assert!(shows_second_thoughts("review it", &rambling));
}
