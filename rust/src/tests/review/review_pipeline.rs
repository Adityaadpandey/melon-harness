//! Ported from tests/test_review_pipeline.py
//! Skipped: review_bench scoring (the Rust port has no bench module).
use super::super::fakes::*;
use super::SERIAL;
use crate::agent::events::AgentEvent;
use crate::agent::review_pipeline::{self as rp, Finding, PipelineEvent, ASK_HOOK};
use crate::agent::verify::CheckResult;
use crate::config::{ApprovalPolicy, Config};
use std::path::Path;
use std::sync::{Arc, Mutex};

fn fnd(sev: &str, at: &str, what: &str, fix: &str, verdict: &str) -> Finding {
    Finding { severity: sev.into(), where_: at.into(), what: what.into(), fix: fix.into(), verdict: verdict.into(), ..Default::default() }
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn sh(dir: &Path, cmd: &str) {
    let ok = std::process::Command::new("sh").arg("-c").arg(cmd).current_dir(dir).output().unwrap().status.success();
    assert!(ok, "failed: {cmd}");
}

/// A git repo with a.ts committed as "x" and then edited to "y", so there is one uncommitted change to review.
fn dirty_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    sh(dir.path(), "git init -q && git config user.email a@b.c && git config user.name t");
    std::fs::write(dir.path().join("a.ts"), "x\n").unwrap();
    sh(dir.path(), "git add . && git commit -qm init");
    std::fs::write(dir.path().join("a.ts"), "y\n").unwrap();
    dir
}

fn review_cfg(dir: &Path) -> Config {
    let mut c = config(dir);
    c.approval = ApprovalPolicy::Auto;
    c.auto_verify = false;
    c.review_run_checks = false;
    c.review_repro = false;
    c
}

/// Installs a fake model for sub-agents for the length of one test and removes it again.
struct Hook;
impl Hook {
    fn set(f: impl Fn(Config, String, String) -> String + Send + Sync + 'static) -> Hook {
        *ASK_HOOK.lock().unwrap() = Some(Arc::new(f));
        Hook
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        *ASK_HOOK.lock().unwrap() = None;
    }
}

#[test]
fn changed_files_skips_deleted_binary_and_follows_renames() {
    let out = " M apps/web/a.tsx\n D old.ts\n?? logo.png\nR  was.ts -> now.ts\n?? pnpm-lock.yaml\n";
    let got = rp::changed_files(out);
    assert_eq!(got[..2], strs(&["apps/web/a.tsx", "now.ts"])[..]);
    assert!(!got.contains(&"logo.png".to_string()) && !got.contains(&"old.ts".to_string()));
}

#[test]
fn files_group_by_area_and_never_exceed_the_reviewer_limit() {
    let files = strs(&["apps/server/a.ts", "apps/server/b.ts", "apps/web/x/y.tsx", "apps/crm/z.tsx", "docs/readme.md", "README.md"]);
    let groups = rp::group_files(&files, 3);
    assert!(groups.len() <= 3);
    let mut all: Vec<String> = groups.iter().flat_map(|(_, fs)| fs.clone()).collect();
    all.sort();
    let mut want = files.clone();
    want.sort();
    assert_eq!(all, want); // nothing lost when areas are folded together
}

#[test]
fn findings_and_verdicts_parse_from_loose_model_output() {
    let found = rp::parse_findings("1. HIGH | a.ts:4 | insert can throw and 500 the list | catch it\nnoise line\n- low | b.ts:2 | stale type | update");
    assert_eq!(found.iter().map(|f| (f.severity.as_str(), f.where_.as_str())).collect::<Vec<_>>(), vec![("HIGH", "a.ts:4"), ("LOW", "b.ts:2")]);
    assert!(rp::parse_findings("NONE").is_empty());
    let got = rp::parse_verdicts("1 | REJECTED | a.ts:9 already wrapped in try/catch | n/a\n2 | CONFIRMED | b.ts:2 no catch | dashboard users", 3);
    assert!(got[0].0 == "REJECTED" && got[0].1.contains("try/catch") && got[1].0 == "CONFIRMED" && got[1].2 == "dashboard users");
    assert_eq!(got[2].0, "UNCLEAR"); // a claim the verifier skipped stays unproven
    assert_eq!(rp::parse_verdicts("no format at all", 1)[0].0, "UNCLEAR");
}

#[test]
fn compose_keeps_confirmed_downgrades_unproven_and_never_lists_rejected_as_findings() {
    let mut a = fnd("HIGH", "a.ts:4", "insert 500s the list", "catch", "CONFIRMED");
    a.evidence = "a.ts:4 awaited, no catch".into();
    a.reach = "dashboard users".into();
    let b = fnd("HIGH", "b.ts:9", "guessed race", "lock", "UNCLEAR");
    let mut c = fnd("MEDIUM", "c.ts:1", "redundant chain", "collapse", "REJECTED");
    c.evidence = "c.ts:1 covers an older server".into();
    let out = rp::compose(&[a, b, c], 2);
    assert!(out.contains("[confirmed] HIGH a.ts:4") && out.contains("affects: dashboard users"));
    assert!(out.contains("[unverified] MEDIUM b.ts:9")); // a HIGH nobody proved is at most MEDIUM
    assert!(out.contains("[rejected] c.ts:1") && !out.contains("[confirmed] MEDIUM c.ts:1"));
}

#[tokio::test]
async fn pipeline_runs_reviewers_then_verifiers_and_hands_the_main_agent_only_what_survived() {
    let _g = SERIAL.lock().await;
    let dir = dirty_repo();
    let prompts: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let p2 = prompts.clone();
    let _h = Hook::set(move |_, prompt, role| {
        p2.lock().unwrap().push((role, prompt.clone()));
        if prompt.contains("claims these defects") {
            "1 | CONFIRMED | a.ts:1 no catch | dashboard\n2 | REJECTED | a.ts:1 handled elsewhere | n/a".into()
        } else {
            "HIGH | a.ts:1 | write can throw and 500 the page | catch\nHIGH | a.ts:1 | a guess about a race | lock".into()
        }
    });
    let mut cfg = review_cfg(dir.path());
    cfg.review_lenses = 4;
    // the answer is built from the verified findings: the model is never called
    let mut agent = scripted(cfg, vec![]).await;
    let events = run(&mut agent, "review the changes").await;
    let notices: Vec<String> = events.iter().filter_map(|e| if let AgentEvent::Notice(n) = e { Some(n.clone()) } else { None }).collect();
    assert!(notices.iter().any(|n| n.contains("Reviewing 1 changed file")), "{notices:?}");
    assert!(notices.iter().any(|n| n.contains("1 confirmed, 1 rejected")), "{notices:?}");
    assert!(agent.session.client.requests.lock().unwrap().is_empty()); // a read-only review is not followed by a model call or a tool loop
    let done: Vec<String> = events.iter().filter_map(|e| if let AgentEvent::TextComplete(c) = e { Some(c.clone()) } else { None }).collect();
    assert_eq!(done.len(), 1);
    assert!(done[0].starts_with("## Code review: 1 issue") && done[0].contains("**1. `a.ts:1`  write can throw and 500 the page**") && done[0].contains("- **Fix:** catch"));
    assert!(done[0].contains("I read 1 file(s)") && done[0].contains("did not run typecheck"));
    let sent: String = agent.session.context_manager.lock().unwrap().messages.iter().filter(|m| m.role == "user").map(|m| m.content.clone()).collect::<Vec<_>>().join(" ");
    assert!(sent.contains("[confirmed] HIGH a.ts:1") && sent.contains("[rejected] a.ts:1"));
    let prompts = prompts.lock().unwrap();
    let verifier_roles: Vec<&String> = prompts.iter().filter(|(_, p)| p.contains("claims these defects")).map(|(r, _)| r).collect();
    assert_eq!(verifier_roles, vec!["default"]); // ONE verifier, on the fast model
    let reviewers: Vec<&String> = prompts.iter().filter(|(_, p)| p.contains("principal engineer reviewing")).map(|(_, p)| p).collect();
    assert_eq!(reviewers.len(), 4); // one reviewer per angle, even for a small diff
    for w in ["LENS: behaviour", "LENS: approach", "LENS: failures", "LENS: rules"] {
        assert!(reviewers.iter().any(|p| p.contains(w)), "no reviewer got {w}");
    }
}

#[tokio::test]
async fn a_named_scope_or_a_clean_tree_skips_the_pipeline() {
    let _g = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let called = Arc::new(Mutex::new(0));
    let c2 = called.clone();
    let _h = Hook::set(move |_, _, _| {
        *c2.lock().unwrap() += 1;
        "NONE".into()
    });
    let mut agent = scripted(review_cfg(dir.path()), vec![text("No issues found"), text("No issues found")]).await;
    run(&mut agent, "review my last 3 commits").await;
    run(&mut agent, "review the changes").await; // not a git repo / nothing changed
    assert_eq!(*called.lock().unwrap(), 0);
}

#[tokio::test]
async fn pipeline_sub_agents_do_not_spawn_their_own_challengers_or_scouts() {
    let _g = SERIAL.lock().await;
    let seen: Arc<Mutex<Vec<Config>>> = Arc::default();
    let s2 = seen.clone();
    let _h = Hook::set(move |cfg, _, _| {
        s2.lock().unwrap().push(cfg);
        String::new()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = review_cfg(dir.path());
    cfg.auto_challenge = true;
    cfg.auto_scout = true;
    rp::ask(&cfg, "p", "review", 4, 5000, None, None, "never").await;
    let c = seen.lock().unwrap()[0].clone();
    assert!(!c.auto_challenge && !c.auto_scout && !c.review_pipeline);
}

#[test]
fn format_answer_is_one_line_per_finding_caps_unproven_ones_and_says_when_nothing_is_left() {
    let f = vec![
        fnd("HIGH", "a.ts:4", "insert 500s the list", "catch", "CONFIRMED"),
        fnd("HIGH", "b.ts:9", "guessed race", "lock", "UNCLEAR"),
        fnd("MEDIUM", "c.ts:1", "redundant chain", "collapse", "REJECTED"),
    ];
    let got = rp::format_answer(&f);
    assert!(got.starts_with("## Code review: 2 issues\n\n### Likely bugs\n**1. `a.ts:4`  insert 500s the list**\n- **Fix:** catch"), "{got}");
    assert!(got.contains("### Not verified, worth a look\n**2. `b.ts:9`  guessed race**\n- **Fix:** lock") && got.ends_with("Want me to fix it?"));
    assert_eq!(rp::format_answer(&[]), "## Code review\n\nNo issues found.");
    assert!(rp::format_answer(&[fnd("LOW", "c.ts:1", "x", "", "REJECTED")]).starts_with("## Code review\n\nNo issues found. I checked 1"));
}

#[tokio::test]
async fn a_reviewer_that_returns_nothing_is_a_failure_and_the_normal_review_loop_runs() {
    let _g = SERIAL.lock().await;
    let dir = dirty_repo();
    let _h = Hook::set(|_, _, _| String::new()); // a crashed or stopped reviewer
    let mut agent = scripted(review_cfg(dir.path()), vec![text("HIGH a.ts:1  found by the normal loop -> fix")]).await;
    let events = run(&mut agent, "review the changes").await;
    let done: Vec<String> = events.iter().filter_map(|e| if let AgentEvent::TextComplete(c) = e { Some(c.clone()) } else { None }).collect();
    assert_eq!(done, vec!["HIGH a.ts:1  found by the normal loop -> fix"]); // not a false "No issues found"
}

#[test]
fn a_self_healing_or_already_present_failure_is_capped_at_medium_and_says_why() {
    let got = rp::parse_verdicts(
        "1 | CONFIRMED | a.ts:4 no catch | every visitor | TEMPORARY (cache entries before deploy, 15 min) | NEW\n2 | CONFIRMED | b.ts:2 no catch | all | ONGOING | ALREADY_AT_HEAD\n3 | CONFIRMED | c.ts:1 x | all | ONGOING | NEW",
        3,
    );
    assert_eq!(got[0].3, "cache entries before deploy, 15 min");
    assert!(got[1].4);
    assert_eq!((got[2].3.as_str(), got[2].4, got[2].5, got[2].6), ("", false, 0, 100));
    let mut a = fnd("HIGH", "a.ts:4", "backfill can 500", "bump the key", "CONFIRMED");
    a.temporary = "15 min".into();
    let mut b = fnd("HIGH", "b.ts:2", "write throws", "catch", "CONFIRMED");
    b.at_head = true;
    let c = fnd("HIGH", "c.ts:1", "proven and lasting", "fix", "CONFIRMED");
    let text = rp::format_answer(&[a, b, c]);
    assert!(text.find("c.ts:1`  proven").unwrap() < text.find("a.ts:4`  backfill").unwrap() && text.contains("goes away by itself (15 min)"));
}

#[test]
fn the_same_cause_in_one_file_is_one_finding_naming_every_line() {
    let a = fnd("HIGH", "svc/read.ts:121-126", "backfill 500s getEventById", "catch the short-link write", "UNCHECKED");
    let b = fnd("HIGH", "svc/read.ts:189-198", "same in getPublicEventDetail", "wrap the short-link write in catch", "UNCHECKED");
    let c = fnd("MEDIUM", "svc/other.ts:3", "unrelated", "catch the short-link write", "UNCHECKED");
    let got = rp::merge_duplicates(vec![a, b, c]);
    assert_eq!(got.iter().map(|f| f.where_.as_str()).collect::<Vec<_>>(), vec!["svc/read.ts:121-126, 189-198", "svc/other.ts:3"]);
}

#[test]
fn the_verifier_can_fold_claims_with_one_cause_into_one_finding_even_across_files() {
    let got = rp::parse_verdicts("1 | CONFIRMED | a.ts:4 no catch | all | ONGOING | NEW | NONE\n2 | CONFIRMED | b.ts:2 same | all | TEMPORARY (15 min) | ALREADY_AT_HEAD | SAME_AS 1", 2);
    assert!(got[1].5 == 1 && got[0].5 == 0);
    let a = fnd("HIGH", "a.ts:4", "no catch", "wrap", "CONFIRMED");
    let mut b = fnd("MEDIUM", "b.ts:2", "same", "wrap", "CONFIRMED");
    b.temporary = "15 min".into();
    b.at_head = true;
    b.same_as = 1;
    let c = fnd("LOW", "c.ts:1", "other", "x", "CONFIRMED");
    let one = rp::merge_same_cause(vec![a, b, c]);
    assert_eq!(one.iter().map(|f| f.where_.as_str()).collect::<Vec<_>>(), vec!["a.ts:4, b.ts:2", "c.ts:1"]);
    assert!(one[0].temporary.is_empty() && !one[0].at_head);
}

#[tokio::test]
async fn a_near_miss_path_is_mended_and_an_unknown_one_is_marked() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("svc/event")).unwrap();
    std::fs::write(dir.path().join("svc/event/disc.ts"), "x").unwrap();
    sh(dir.path(), "git init -q && git add -A");
    let mut f = vec![fnd("HIGH", "svc/disc.ts:9", "w", "f", ""), fnd("HIGH", "svc/nope.ts:1", "w", "f", ""), fnd("HIGH", "svc/event/disc.ts:2", "w", "f", "")];
    rp::fix_paths(&mut f, dir.path()).await;
    assert_eq!(f.iter().map(|x| x.where_.as_str()).collect::<Vec<_>>(), vec!["svc/event/disc.ts:9", "svc/nope.ts:1 (file not found)", "svc/event/disc.ts:2"]);
}

#[tokio::test]
async fn a_file_moved_into_a_new_folder_is_one_move_line_not_a_deletion_plus_a_new_file() {
    use crate::agent::review::git;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    sh(p, "git init -q && git config user.email t@t && git config user.name t");
    std::fs::create_dir_all(p.join("events")).unwrap();
    std::fs::write(p.join("events/page.tsx"), "export default function P() {}\n").unwrap();
    std::fs::write(p.join("events/other.tsx"), "old\n").unwrap();
    sh(p, "git add . && git commit -qm base");
    std::fs::create_dir_all(p.join("events/(list)")).unwrap();
    std::fs::rename(p.join("events/page.tsx"), p.join("events/(list)/page.tsx")).unwrap();
    std::fs::write(p.join("events/other.tsx"), "new\n").unwrap();

    let porcelain = git(p, &["status", "--porcelain", "-uall"]).await;
    let (moves, deleted, added) = rp::pair_moves(p, &rp::deleted_files(&porcelain), &rp::changed_files(&porcelain)).await;
    assert_eq!(moves, vec![("events/page.tsx".to_string(), "events/(list)/page.tsx".to_string())]);
    assert!(deleted.is_empty());
    assert_eq!(added, strs(&["events/other.tsx"]));
    std::fs::create_dir_all(p.join("events/[id]")).unwrap();
    std::fs::write(p.join("events/[id]/page.tsx"), "x\n").unwrap();
    sh(p, "git add 'events/[id]'");
    let stayed = rp::left_behind(p, &moves).await;
    assert!(stayed.contains(&"events/other.tsx".to_string()) && stayed.contains(&"events/[id]/page.tsx".to_string()) && !stayed.contains(&"events/page.tsx".to_string()));
    let out = rp::frame("No issues found.", "Moves the list page into a route group.", &added, &moves, &deleted, &[], &[], "");
    assert!(out.contains("events/page.tsx -> events/(list)/page.tsx") && out.contains("did not run typecheck") && out.contains("git add -A") && out.starts_with("**What the change does"));
}

#[test]
fn review_context_brings_the_folders_rules_and_the_matching_playbook() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("apps/web")).unwrap();
    std::fs::write(p.join("AGENTS.md"), "Never log auth tokens anywhere in the server code.\n").unwrap();
    std::fs::write(p.join("apps/web/AGENTS.md"), "Use pnpm, never npm, in the web app scripts.\n").unwrap();
    std::fs::write(p.join("apps/web/a.tsx"), "x\n").unwrap();
    let ctx = crate::agent::review::review_context(&config(p), "review the changes", &strs(&["apps/web/a.tsx"]));
    assert!(ctx.contains("Never log auth tokens") && ctx.contains("Use pnpm") && !ctx.contains("shown once"));
    assert!(ctx.find("Never log").unwrap() < ctx.find("Use pnpm").unwrap()); // root first, the deeper folder after
}

#[test]
fn duplicate_findings_from_several_angles_name_the_place_once() {
    let found: Vec<Finding> = (0..4).map(|_| fnd("HIGH", "a.ts:1", "write can throw and 500 the page", "catch it", "UNCHECKED")).collect();
    let merged = rp::merge_duplicates(found);
    assert!(merged.len() == 1 && merged[0].where_ == "a.ts:1");
}

#[test]
fn by_default_one_reviewer_carries_every_angle_beside_a_contract_tracer_and_a_pinned_count_fans_out() {
    let cfg = Config::default();
    let pick = |c: &Config, diff: &str, files: &[&str], ctx: &str| rp::pick_lenses(c, diff, &strs(files), ctx);
    let names = |v: Vec<(String, String)>| v.into_iter().map(|(n, _)| n).collect::<Vec<_>>();
    let big = "+x\n".repeat(300);
    let (name, text) = pick(&cfg, &big, &["a.ts", "b.ts", "c.ts"], "PROJECT RULES")[0].clone();
    assert_eq!(name, "review");
    assert_eq!(names(pick(&cfg, &big, &["a.ts"], "")), vec!["review", "contracts"]);
    assert!(["LENS: behaviour", "LENS: approach", "LENS: failures", "LENS: rules"].iter().all(|w| text.contains(w)) && !text.contains("LENS: history"));
    assert!(pick(&cfg, "+x\n", &["a.ts"], "")[0].1.contains("LENS: history")); // no rules to audit: read the history instead
    assert!(pick(&cfg, "+check the jwt token\n", &["auth.ts"], "")[0].1.contains("LENS: security"));
    let mut three = Config::default();
    three.review_lenses = 3;
    assert_eq!(names(pick(&three, "+x\n", &["a.ts"], "")), vec!["behaviour", "approach", "failures"]);
    let mut one = Config::default();
    one.review_lenses = 1;
    assert_eq!(names(pick(&one, "+jwt\n", &["auth.ts"], "")), vec!["behaviour"]);
}

#[test]
fn verdicts_carry_a_confidence_and_a_low_one_is_dropped_from_the_answer() {
    let got = rp::parse_verdicts("1 | CONFIRMED | a.ts:4 no catch | users | ONGOING | NEW | NONE | CONF 45\n2 | CONFIRMED | b.ts:1 x | all | ONGOING | NEW | NONE", 2);
    assert!(got[0].6 == 45 && got[1].6 == 100); // a reply without a score reads as sure
}

#[tokio::test]
async fn a_claim_the_verifier_scores_below_the_cutoff_is_ruled_out_and_remembered() {
    let _g = SERIAL.lock().await;
    let dir = dirty_repo();
    let _h = Hook::set(|_, prompt, _| {
        if prompt.contains("claims these defects") {
            "1 | CONFIRMED | a.ts:1 maybe | few | ONGOING | NEW | NONE | CONF 30".into()
        } else {
            "MEDIUM | a.ts:1 | a rare race | lock".into()
        }
    });
    let mut cfg = review_cfg(dir.path());
    cfg.review_lenses = 1;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rp::run_review_pipeline(cfg, "review the changes".into(), tx).await;
    let mut answer = None;
    while let Ok(e) = rx.try_recv() {
        if let PipelineEvent::Answer(a) = e {
            answer = Some(a);
        }
    }
    let answer = answer.expect("the pipeline gave an answer");
    assert!(answer.starts_with("## Code review\n\nNo issues found.") && answer.contains("checked 1 suspected"));
    // a low score is "not worth reporting", not "proven wrong": it is not remembered
    assert_eq!(crate::agent::review_memory::render(dir.path(), None), "");
}

#[test]
fn check_findings_blame_only_failures_that_name_a_changed_file() {
    let bad = CheckResult { command: "npx tsc --noEmit".into(), ok: false, exit_code: Some(2), seconds: 4.0, output_tail: "src/a.ts(12,5): error TS2322: nope\nsrc/old.ts(3,1): error TS1".into() };
    let fine = CheckResult { command: "lint".into(), ok: true, exit_code: Some(0), seconds: 1.0, output_tail: String::new() };
    let got = rp::check_findings(&[bad.clone(), fine], &strs(&["src/a.ts"]));
    assert_eq!(got.iter().map(|f| (f.where_.as_str(), f.verdict.as_str())).collect::<Vec<_>>(), vec![("src/a.ts:12", "CONFIRMED")]);
    assert!(rp::check_findings(&[bad], &strs(&["src/other.ts"])).is_empty()); // old debt elsewhere is not this change's
}

#[test]
fn claude_code_runs_read_only_with_the_repos_secret_rules_and_its_json_is_parsed() {
    use crate::agent::review_claude as rc;
    let mut cfg = Config::default();
    cfg.review_claude_model = "sonnet".into();
    let cmd = rc::build_command(&cfg, "hello", "sonnet");
    assert_eq!(cmd[..3], strs(&["claude", "-p", "hello"])[..]);
    let dis = cmd.iter().position(|c| c == "--disallowedTools").unwrap();
    assert!(cmd[dis..].contains(&"Edit".to_string()) && cmd.contains(&"Bash(git blame:*)".to_string()));
    assert!(cmd.iter().any(|c| c.starts_with("Read(**/.env")));
    assert_eq!(cmd[cmd.len() - 2..], strs(&["--model", "sonnet"])[..]);
    assert_eq!(rc::parse_output(r#"{"result": "SUMMARY: x\nNONE", "usage": {"input_tokens": 5, "output_tokens": 7}}"#), ("SUMMARY: x\nNONE".to_string(), 12));
    assert_eq!(rc::parse_output(r#"{"is_error": true, "result": "boom"}"#), (String::new(), 0));
    assert_eq!(rc::parse_output("not json"), (String::new(), 0));
}

#[test]
fn review_memory_keeps_a_ruled_out_claim_once_and_shows_it_to_the_next_review() {
    use crate::agent::review_memory as rm;
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("d");
    let row = |at: &str| vec![(at.to_string(), "unguarded write".to_string(), "caller wraps it".to_string())];
    rm::remember(dir.path(), &row("a.ts:4"), Some(&base));
    rm::remember(dir.path(), &row("a.ts:9"), Some(&base)); // same file and wording
    assert_eq!(rm::load(dir.path(), Some(&base)).len(), 1);
    assert!(rm::render(dir.path(), Some(&base)).contains("a.ts:4: unguarded write"));
    assert_eq!(rm::render(&dir.path().join("x"), Some(&dir.path().join("e"))), "");
}

#[tokio::test]
async fn review_targets_resolve_last_commits_a_commit_and_the_branch() {
    use crate::agent::review_target as rt;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    sh(p, "git init -q -b main && git config user.email a@b.c && git config user.name t");
    for i in 0..3 {
        std::fs::write(p.join(format!("f{i}.ts")), format!("{i}\n")).unwrap();
        sh(p, &format!("git add . && git commit -qm c{i}"));
    }
    let t = rt::resolve(p, "review my last 2 commits", &[]).await.expect("last 2 commits");
    let mut files = t.files.clone();
    files.sort();
    assert!(t.rev == strs(&["HEAD~2", "HEAD"]) && files == strs(&["f1.ts", "f2.ts"]) && !t.includes_worktree());
    let sha = String::from_utf8(std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).current_dir(p).output().unwrap().stdout).unwrap();
    let one = rt::resolve(p, &format!("review commit {}", sha.trim()), &[]).await.expect("one commit");
    assert_eq!(one.files, strs(&["f2.ts"]));
    assert!(rt::resolve(p, "review this branch", &[]).await.is_none()); // on main there is no branch to compare
    sh(p, "git checkout -q -b feat");
    std::fs::write(p.join("g.ts"), "new\n").unwrap();
    sh(p, "git add . && git commit -qm g");
    let br = rt::resolve(p, "review this branch", &[]).await.expect("branch");
    assert!(br.files == strs(&["g.ts"]) && br.includes_worktree());
    assert!(rt::resolve(p, "review the changes", &[]).await.is_none()); // the working tree is the default, not a target
    assert!(rt::resolve(p, "review my last 9 commits", &[]).await.is_none()); // more commits than exist
}
