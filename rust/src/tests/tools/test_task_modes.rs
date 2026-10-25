//! Ported from tests/test_task_modes.py: each kind of request pulls in the playbook(s) for it, and mixed requests
//! pull in several.
use crate::agent::skills::{match_skills, parse_skill, Skill};
use crate::agent::task::TaskStatus;
use crate::config::ApprovalPolicy;

/// The playbooks that ship with the harness (the repo's skills/ folder).
fn builtin() -> Vec<Skill> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("skills");
    let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "md")).collect();
    files.sort();
    files.iter().filter_map(|f| parse_skill(&std::fs::read_to_string(f).unwrap(), Some(f))).collect()
}

fn names(request: &str) -> Vec<String> {
    match_skills(&builtin(), request, 4).into_iter().map(|s| s.name).collect()
}

#[test]
fn one_request_one_lens() {
    let cases = [
        ("review pull request 42", "review-pr"),
        ("review the last commit", "review-commits"),
        ("look over my changes before I commit", "review-changes"),
        ("find bugs in the checkout flow", "bug-hunt"),
        ("will this scale to a million users", "scalability"),
        ("how should we add a notifications feature", "feature-planning"),
        ("the dashboard is slow, profile it", "performance"),
        ("check the upload endpoint for vulnerabilities", "security"),
    ];
    for (request, want) in cases {
        assert!(names(request).contains(&want.to_string()), "{request:?} -> {:?}", names(request));
    }
}

#[test]
fn mixed_request_loads_every_lens() {
    let got = names("review this pull request for security and performance");
    for want in ["review-pr", "security", "performance"] {
        assert!(got.contains(&want.to_string()), "{want} missing in {got:?}");
    }
}

#[test]
fn review_commits_beats_the_bare_word_commit() {
    let got = names("review the last commit");
    let at = |n: &str| got.iter().position(|g| g == n);
    assert!(at("review-commits").unwrap() < at("git-workflow").unwrap_or(99));
}

#[test]
fn plain_chat_loads_nothing() {
    assert!(names("hello there").is_empty());
}

#[test]
fn repairing_can_repeat() {
    assert!(TaskStatus::Repairing.allowed(TaskStatus::Repairing));
}

#[test]
fn shift_tab_cycles_modes_and_skips_yolo() {
    let order = crate::ui::app::MODES;
    assert!(!order.contains(&ApprovalPolicy::Yolo) && !order.contains(&ApprovalPolicy::Never));
    assert!(order[0] == ApprovalPolicy::OnRequest && order.contains(&ApprovalPolicy::Plan));
}
