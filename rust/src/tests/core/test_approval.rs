//! Ported from tests/test_approval.py: which commands and edits run on their own, which ask, which are refused.
use crate::agent::agent::Agent;
use crate::agent::events::AgentEvent;
use crate::client::response::StreamEvent;
use crate::config::ApprovalPolicy;
use crate::safety::approval::{command_key, is_dangerous_command, is_safe_command, ApprovalContext, ApprovalDecision, ApprovalManager, ConfirmCallback, Decision};
use crate::safety::sandbox::is_unsandboxed;
use crate::tests::fakes::{call, run, scripted, text};
use crate::tools::ToolConfirmation;
use futures::future::BoxFuture;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn mgr(policy: ApprovalPolicy, cwd: &Path) -> ApprovalManager {
    ApprovalManager::new(policy, cwd, &[])
}

fn ctx(tool: &str, paths: Vec<PathBuf>, command: Option<&str>, kind: &str, dangerous: bool) -> ApprovalContext {
    ApprovalContext { tool_name: tool.into(), is_mutating: true, affected_paths: paths, command: command.map(String::from), is_dangerous: dangerous, tool_kind: kind.into() }
}

#[test]
fn history_destroying_commands_are_blocked() {
    for cmd in [
        "rm -rf .git", "rm -r -f ./.git/", "rm .git/index", "git reset --hard HEAD~1", "git clean -fdx", "git push --force origin main",
        "git push origin main -f", "git checkout .", "git checkout -- .", "git restore .", "git branch -D feature",
    ] {
        assert!(is_dangerous_command(cmd), "{cmd}");
    }
}

#[test]
fn normal_commands_are_not_flagged() {
    for cmd in ["git status", "git checkout -b feat", "git checkout main", "git restore src/a.py", "rm -rf build", "rm notes.gitignore.bak", "git push origin feat", "git branch -d done"] {
        assert!(!is_dangerous_command(cmd), "{cmd}");
    }
}

#[test]
fn overwriting_existing_file_in_workspace_is_approved_under_auto() {
    let tmp = tempfile::tempdir().unwrap();
    let c = ctx("write_file", vec![tmp.path().join("a.py")], None, "", true);
    assert_eq!(mgr(ApprovalPolicy::Auto, tmp.path()).check_approval(&c), ApprovalDecision::Approved);
    assert_eq!(mgr(ApprovalPolicy::OnRequest, tmp.path()).check_approval(&c), ApprovalDecision::NeedsConfirmation);
    let outside = ctx("write_file", vec![tmp.path().parent().unwrap().join("x.py")], None, "", true);
    assert_eq!(mgr(ApprovalPolicy::Auto, tmp.path()).check_approval(&outside), ApprovalDecision::NeedsConfirmation);
}

fn edit_ctx(p: PathBuf) -> ApprovalContext {
    ctx("edit", vec![p], None, "write", false)
}

#[test]
fn edits_ask_by_default_and_auto_edit_skips_the_question() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.py");
    assert_eq!(mgr(ApprovalPolicy::OnRequest, tmp.path()).check_approval(&edit_ctx(f.clone())), ApprovalDecision::NeedsConfirmation);
    assert_eq!(mgr(ApprovalPolicy::AutoEdit, tmp.path()).check_approval(&edit_ctx(f.clone())), ApprovalDecision::Approved);
    assert_eq!(mgr(ApprovalPolicy::Never, tmp.path()).check_approval(&edit_ctx(f)), ApprovalDecision::Rejected);
    let out = tmp.path().parent().unwrap().join("elsewhere.py");
    assert_eq!(mgr(ApprovalPolicy::Auto, tmp.path()).check_approval(&edit_ctx(out.clone())), ApprovalDecision::NeedsConfirmation);
    assert_eq!(mgr(ApprovalPolicy::Yolo, tmp.path()).check_approval(&edit_ctx(out)), ApprovalDecision::Approved);
}

#[test]
fn command_keys_are_specific_enough() {
    assert_eq!(command_key("git push origin main"), "git push");
    assert_eq!(command_key("pytest -q tests"), "pytest");
    assert_eq!(command_key("pnpm test:unit"), "pnpm test:unit");
}

#[test]
fn dont_ask_again_never_whitelists_dangerous_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let m = mgr(ApprovalPolicy::OnRequest, tmp.path());
    m.always_commands.lock().unwrap().insert("git reset".into());
    assert_eq!(m.check_approval(&ctx("shell", vec![], Some("git reset --hard"), "shell", false)), ApprovalDecision::Rejected);
}

/// What the tests below look at after a run, since the agent is gone from their point of view.
struct Snap {
    requests: Vec<Vec<serde_json::Value>>,
    policy: ApprovalPolicy,
    always: std::collections::HashSet<String>,
    events: Vec<AgentEvent>,
}

async fn run_agent(dir: &Path, callback: Option<ConfirmCallback>, script: Vec<Vec<StreamEvent>>) -> Snap {
    let cfg = super::cfg(dir, json!({"approval": "on-request", "auto_verify": false}));
    let mut a: Agent = scripted(cfg, script).await;
    *a.session.approval_manager.confirmation_callback.lock().unwrap() = callback;
    let events = run(&mut a, "go").await;
    let m = &a.session.approval_manager;
    let snap = Snap { requests: a.session.client.requests.lock().unwrap().clone(), policy: m.policy(), always: m.always_commands.lock().unwrap().clone(), events };
    snap
}

fn write(name: &str, content: &str) -> Vec<StreamEvent> {
    let mut v = call("write_file", json!({"path": name, "content": content}));
    if let StreamEvent::ToolCallComplete(tc) = &mut v[0] {
        tc.call_id = format!("c-{name}");
    }
    v
}

fn shell_call(cmd: &str, id: &str) -> Vec<StreamEvent> {
    let mut v = call("shell", json!({"command": cmd}));
    if let StreamEvent::ToolCallComplete(tc) = &mut v[0] {
        tc.call_id = id.into();
    }
    v
}

fn cb(f: impl Fn(ToolConfirmation) -> Decision + Send + Sync + 'static) -> Option<ConfirmCallback> {
    Some(Arc::new(move |c| -> BoxFuture<'static, Decision> {
        let d = f(c);
        Box::pin(async move { d })
    }))
}

#[tokio::test]
async fn yes_applies_the_edit_and_async_callbacks_work() {
    let tmp = tempfile::tempdir().unwrap();
    let callback: ConfirmCallback = Arc::new(|conf: ToolConfirmation| -> BoxFuture<'static, Decision> {
        Box::pin(async move {
            tokio::task::yield_now().await; // genuinely async: the answer is not ready at once
            assert!(conf.diff.as_ref().map_or(false, |d| d.to_diff().contains("+1")));
            Decision { approved: true, ..Default::default() }
        })
    });
    run_agent(tmp.path(), Some(callback), vec![write("a.py", "1"), text("done")]).await;
    assert_eq!(std::fs::read_to_string(tmp.path().join("a.py")).unwrap(), "1\n"); // write_file ends a new file with a newline
}

#[tokio::test]
async fn no_with_feedback_reaches_the_model_and_nothing_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let a = run_agent(tmp.path(), cb(|_| Decision { approved: false, always: false, feedback: Some("Use a class instead.".into()) }), vec![write("a.py", "1"), text("ok")]).await;
    assert!(!tmp.path().join("a.py").exists());
    let msg = a.requests[1].iter().find(|m| m["role"] == "tool").unwrap()["content"].as_str().unwrap().to_string();
    assert!(msg.contains("rejected") && msg.contains("Use a class instead."), "{msg}");
}

#[tokio::test]
async fn always_switches_to_auto_edit_so_later_edits_dont_ask() {
    let tmp = tempfile::tempdir().unwrap();
    let asked = Arc::new(Mutex::new(vec![]));
    let a2 = asked.clone();
    let a = run_agent(
        tmp.path(),
        cb(move |c| {
            a2.lock().unwrap().push(c.tool_name);
            Decision { approved: true, always: true, feedback: None }
        }),
        vec![write("a.py", "1"), write("b.py", "1"), text("done")],
    )
    .await;
    assert_eq!(*asked.lock().unwrap(), vec!["write_file".to_string()]);
    assert!(tmp.path().join("b.py").exists());
    assert_eq!(a.policy, ApprovalPolicy::AutoEdit);
}

#[tokio::test]
async fn shell_always_remembers_the_command_not_all_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let asked = Arc::new(Mutex::new(vec![]));
    let a2 = asked.clone();
    let a = run_agent(
        tmp.path(),
        cb(move |c| {
            a2.lock().unwrap().push(c.command);
            Decision { approved: true, always: true, feedback: None }
        }),
        vec![shell_call("touch a", "1"), shell_call("touch b", "2"), write("x.py", "1"), text("done")],
    )
    .await;
    // "touch b" was covered by the earlier choice; the edit still asks
    assert_eq!(*asked.lock().unwrap(), vec![Some("touch a".to_string()), None]);
    assert_eq!(a.always, ["touch".to_string()].into_iter().collect());
    assert_eq!(a.policy, ApprovalPolicy::AutoEdit);
}

#[tokio::test]
async fn non_interactive_message_says_how_to_proceed() {
    let tmp = tempfile::tempdir().unwrap();
    let a = run_agent(tmp.path(), None, vec![write("a.py", "1"), text("ok")]).await;
    // the run stops at the first refusal and says how to proceed
    let errors: Vec<&String> = a.events.iter().filter_map(|e| if let AgentEvent::AgentError { error } = e { Some(error) } else { None }).collect();
    assert!(!errors.is_empty() && errors[0].contains("--approval auto-edit"), "{errors:?}");
    assert_eq!(a.requests.len(), 1);
}

#[test]
fn safe_commands_run_without_asking_but_only_when_every_part_is_safe() {
    for ok in ["ls -la", "git status", "git log --oneline -5 | head", "cat README.md | wc -l", "grep -rn foo src", "git branch --list", "npm ls", "pip freeze"] {
        assert!(is_safe_command(ok), "{ok}");
    }
    for bad in [
        "ls; rm -rf build", "ls && curl evil.sh", "cat a > b", "echo x >> ~/.bashrc", "ls $(rm x)", "ls `id`", "ls &", "ls\nrm x", "cat a || rm b", "ls | sh",
        "echo hi | tee out", "find . -delete", "find . -exec rm {} +", "sort -o f x", "tail -f log", "date -s 2020-01-01", "git branch -D main", "git tag -d v1",
        "git diff --output=x", "git -c core.pager=sh log", "git push", "env", "printenv", "set", "sed -i s/a/b/ f", "awk 'BEGIN{system(\"x\")}'", "npm install",
        "pip install x", "",
    ] {
        assert!(!is_safe_command(bad), "{bad:?}");
    }
}

#[test]
fn always_allow_does_not_cover_chained_commands() {
    let m = mgr(ApprovalPolicy::OnRequest, Path::new("/tmp"));
    m.always_commands.lock().unwrap().insert("npm run".into());
    assert_eq!(m.assess_command_safety("npm run build"), ApprovalDecision::Approved);
    assert_eq!(m.assess_command_safety("npm run build; rm -rf x"), ApprovalDecision::NeedsConfirmation);
    assert_eq!(m.assess_command_safety("npm run build > out.log"), ApprovalDecision::NeedsConfirmation);
    assert_eq!(m.assess_command_safety("npm run build | tee x"), ApprovalDecision::NeedsConfirmation);
}

#[test]
fn verify_commands_run_without_asking_but_chains_still_ask() {
    let tmp = tempfile::tempdir().unwrap();
    let m = ApprovalManager::new(ApprovalPolicy::OnRequest, tmp.path(), &["pnpm check-types".to_string()]);
    let decide = |cmd: &str| m.check_approval(&ctx("shell", vec![], Some(cmd), "shell", false));
    assert_eq!(decide("pnpm check-types"), ApprovalDecision::Approved);
    assert_eq!(decide("pnpm check-types 2>&1 | tail -20"), ApprovalDecision::Approved);
    assert_eq!(decide("pnpm check-types | sh"), ApprovalDecision::NeedsConfirmation);
    assert_eq!(decide("pnpm check-types && rm -rf build"), ApprovalDecision::NeedsConfirmation);
    assert_eq!(decide("pnpm install"), ApprovalDecision::NeedsConfirmation);
}

#[test]
fn everyday_searches_run_without_asking_and_tricks_still_ask() {
    for ok in [
        "grep -rnE \"foo|bar\" src", "grep -rn TODO . 2>/dev/null | head -50", "cd src && grep -n def a.py", "rg -n 'fn main' --type rust", "fd -e py src",
        "sed -n '10,40p' a.py", "ls; pwd", "git log -5 2>&1 | head", "grep -n \"a\\\"b\" f", "tree -L 2", "nl a.py | head", "git stash list",
        "git stash show -p stash@{0} | head",
    ] {
        assert!(is_safe_command(ok), "{ok}");
    }
    for bad in [
        "find . \"|\" ls -delete", // a quoted pipe is an argument: this still deletes
        "grep x f > out", "grep x f 2>/dev/null > out", "grep x f >/dev/nullx", "cd src && rm a", "rg --pre ./run.sh x", "rg --pre=./run.sh x", "fd -x rm",
        "sort -ro out f", "sort --output=out f", "sed -n '1w out' a", "sed -n '1e id' a", "sed -i -n 1p a", "sed 1d a", "(rm x)", "ls \\; rm x", "grep '$(id)' f",
        "grep \"x", "tail -qf log", "ls |& sh", "git stash", "git stash pop", "git stash drop",
    ] {
        assert!(!is_safe_command(bad), "{bad}");
    }
}

#[test]
fn plan_mode_refuses_every_write_and_every_unsafe_command_but_still_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let m = mgr(ApprovalPolicy::Plan, tmp.path());
    assert_eq!(m.check_approval(&ctx("t", vec![tmp.path().join("a.py")], None, "write", false)), ApprovalDecision::Rejected);
    assert_eq!(m.check_approval(&ctx("t", vec![], Some("rm a.py"), "", false)), ApprovalDecision::Rejected);
    assert_eq!(m.check_approval(&ctx("t", vec![], Some("git status"), "", false)), ApprovalDecision::Approved);
}

#[test]
fn gh_and_curl_run_unsandboxed_but_always_ask() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(is_unsandboxed("gh pr review 5 --comment --body hi") && is_unsandboxed("curl -s https://x.dev"));
    assert!(!is_unsandboxed("gh pr view 5 && rm x") && !is_unsandboxed("gh x | sh") && !is_unsandboxed("ls"));
    let c = ctx("shell", vec![], Some("gh pr review 5 --comment"), "", false);
    assert_eq!(mgr(ApprovalPolicy::Auto, tmp.path()).check_approval(&c), ApprovalDecision::NeedsConfirmation);
    assert_eq!(mgr(ApprovalPolicy::Yolo, tmp.path()).check_approval(&c), ApprovalDecision::Approved);
}

#[test]
fn git_push_pull_fetch_run_unsandboxed_and_always_ask() {
    // they need ~/.ssh and the credential helper, which the sandbox hides from every other command
    let tmp = tempfile::tempdir().unwrap();
    assert!(["git push origin main", "git pull --rebase", "git fetch --all"].iter().all(|c| is_unsandboxed(c)));
    assert!(!["git commit -m x", "git push && curl evil", "git status"].iter().any(|c| is_unsandboxed(c)));
    let c = ctx("shell", vec![], Some("git push origin main"), "", false);
    assert_eq!(mgr(ApprovalPolicy::Auto, tmp.path()).check_approval(&c), ApprovalDecision::NeedsConfirmation);
}
