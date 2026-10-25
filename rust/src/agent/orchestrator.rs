//! Dependency-aware parallel execution. Each task runs in its own git worktree on its own branch, so workers can never
//! corrupt each other's files or the user's working tree. Results are merged into an integration branch in dependency
//! order. Overlapping edits are detected, real merge conflicts are never resolved by guessing: the conflicted task is
//! re-run on top of the integrated state with the original request in view, and the report says so. The user's checkout
//! is only touched with apply=True and only when it is clean.
use crate::agent::agent::Agent;
use crate::agent::events::AgentEvent;
use crate::agent::verify::{run_check, select_checks};
use crate::config::Config;
use crate::tools::ProgressFn;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Seconds one worker may take before it is failed and the rest carry on.
const WORKER_TIMEOUT: u64 = 900;

#[derive(Debug)]
pub struct GraphError(pub String);

#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    pub goal: String,
    pub deps: Vec<String>,
    /// Ownership hint, not a lock.
    pub files: Vec<String>,
    /// pending|done|failed|conflict|resolved|skipped
    pub status: String,
    pub branch: String,
    pub changed: Vec<String>,
    pub seconds: f64,
    pub note: String,
    /// The worker's own verification outcome.
    pub task_status: Option<String>,
}

pub fn parse_graph(data: &Value) -> Result<Vec<Node>, GraphError> {
    let strs = |v: &Value| -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
    let nodes: Vec<Node> = data["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| Node { id: t["id"].as_str().unwrap_or("").into(), goal: t["goal"].as_str().unwrap_or("").into(), deps: strs(&t["deps"]), files: strs(&t["files"]), status: "pending".into(), branch: String::new(), changed: vec![], seconds: 0.0, note: String::new(), task_status: None })
        .collect();
    if nodes.is_empty() {
        return Err(GraphError("graph has no tasks".into()));
    }
    let ids: HashSet<&String> = nodes.iter().map(|n| &n.id).collect();
    if ids.len() != nodes.len() {
        return Err(GraphError("duplicate task ids".into()));
    }
    for n in &nodes {
        for d in &n.deps {
            if !ids.contains(d) {
                return Err(GraphError(format!("{} depends on unknown task {d}", n.id)));
            }
        }
    }
    waves(&nodes)?; // raises on cycles
    Ok(nodes)
}

/// Topological layers (indices): every node's deps sit in an earlier layer.
pub fn waves(nodes: &[Node]) -> Result<Vec<Vec<usize>>, GraphError> {
    let (mut done, mut remaining, mut out): (HashSet<String>, Vec<usize>, Vec<Vec<usize>>) = (HashSet::new(), (0..nodes.len()).collect(), vec![]);
    while !remaining.is_empty() {
        let layer: Vec<usize> = remaining.iter().copied().filter(|i| nodes[*i].deps.iter().all(|d| done.contains(d))).collect();
        if layer.is_empty() {
            return Err(GraphError(format!("dependency cycle among: {}", remaining.iter().map(|i| nodes[*i].id.clone()).collect::<Vec<_>>().join(", "))));
        }
        done.extend(layer.iter().map(|i| nodes[*i].id.clone()));
        remaining.retain(|i| !layer.contains(i));
        out.push(layer);
    }
    Ok(out)
}

async fn git(cwd: &Path, args: &[&str]) -> (i32, String) {
    match Command::new("git").args(args).current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped()).output().await {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.code().unwrap_or(1), s.trim().to_string())
        }
        Err(e) => (1, e.to_string()),
    }
}

pub struct Orchestrator {
    config: Config,
    sem: Arc<tokio::sync::Semaphore>,
    git_lock: Arc<tokio::sync::Mutex<()>>,
    run_id: String,
    tmp: PathBuf,
    repo: PathBuf,
    integration: String,
    int_dir: PathBuf,
    worktrees: std::sync::Mutex<Vec<PathBuf>>,
    progress: Option<ProgressFn>,
    overlaps: BTreeMap<String, Vec<String>>,
    combined_checks: Vec<Value>,
}

impl Orchestrator {
    pub fn new(config: Config, max_workers: usize, progress: Option<ProgressFn>) -> Orchestrator {
        let run_id = uuid::Uuid::new_v4().simple().to_string()[..6].to_string();
        let tmp = tempfile::Builder::new().prefix(&format!("melon-{run_id}-")).tempdir().map(|t| t.keep()).unwrap_or_else(|_| std::env::temp_dir().join(format!("melon-{run_id}")));
        Orchestrator {
            repo: config.cwd.clone(),
            config,
            sem: Arc::new(tokio::sync::Semaphore::new(max_workers.max(1))),
            git_lock: Arc::new(tokio::sync::Mutex::new(())),
            int_dir: tmp.join("integration"),
            tmp,
            run_id,
            integration: String::new(),
            worktrees: Default::default(),
            progress,
            overlaps: BTreeMap::new(),
            combined_checks: vec![],
        }
    }

    async fn worktree(&self, name: &str, base: &str) -> Result<PathBuf, GraphError> {
        let path = self.tmp.join(name);
        let _g = self.git_lock.lock().await; // worktree add/remove touch shared .git metadata
        let (code, out) = git(&self.repo, &["worktree", "add", "-q", "-b", &format!("melon/{}/{name}", self.run_id), &path.to_string_lossy(), base]).await;
        if code != 0 {
            return Err(GraphError(format!("git worktree failed: {out}")));
        }
        self.worktrees.lock().unwrap().push(path.clone());
        Ok(path)
    }

    async fn run_worker(&self, node: &mut Node, request: &str, base: &str) {
        let Ok(_permit) = self.sem.acquire().await else { return };
        let started = Instant::now();
        node.branch = format!("melon/{}/{}", self.run_id, node.id);
        let wt = match self.worktree(&node.id, base).await {
            Ok(w) => w,
            Err(e) => {
                node.status = "failed".into();
                node.note = e.0;
                return;
            }
        };
        let git_dir = crate::util::paths::resolve(&self.repo.join(".git")).to_string_lossy().to_string(); // a worktree's git metadata lives in the main repo's .git
        let mut cfg = self.config.clone();
        cfg.cwd = wt.clone();
        cfg.sandbox_write_extra.push(git_dir);
        let owned = if node.files.is_empty() { String::new() } else { format!("\nFiles you own: {}. Do not edit other files.", node.files.join(", ")) };
        let prompt = format!(
            "You are one worker in a parallel job. Overall request (for context only):\n{request}\n\nYOUR task: {}{owned}\nOther workers handle the rest in separate checkouts; do only your task, keep the change small.",
            node.goal
        );
        let key = format!("{}:{}", self.run_id, node.id);
        let say = |info: Value| {
            if let Some(p) = &self.progress {
                let mut v = json!({"agent": node.id});
                if let (Some(a), Some(b)) = (v.as_object_mut(), info.as_object()) {
                    a.extend(b.clone());
                }
                p(&key, v);
            }
        };
        say(json!({"kind": "start", "goal": node.goal}));
        let mut last_text = String::new();
        let work = async {
            let mut agent = Agent::new(cfg, None).await;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
            let mut task_status = None;
            {
                let run = agent.run(&prompt, tx);
                tokio::pin!(run);
                let mut handle = |ev: AgentEvent, last: &mut String, ts: &mut Option<String>| match ev {
                    AgentEvent::ToolCallStart { name, arguments, tokens, .. } => say(json!({"kind": "tool", "name": name, "args": arguments, "tokens": tokens})),
                    AgentEvent::TextComplete(c) => *last = c,
                    AgentEvent::AgentEnd { report, .. } => *ts = report["status"].as_str().map(String::from),
                    _ => {}
                };
                loop {
                    tokio::select! {
                        _ = &mut run => break,
                        Some(ev) = rx.recv() => handle(ev, &mut last_text, &mut task_status),
                    }
                }
                while let Ok(ev) = rx.try_recv() {
                    handle(ev, &mut last_text, &mut task_status);
                }
            }
            agent.close().await;
            task_status
        };
        // one stuck worker must not hold the whole layer (and the merge) hostage
        match tokio::time::timeout(Duration::from_secs(WORKER_TIMEOUT), work).await {
            Ok(ts) => {
                node.task_status = ts;
                if matches!(node.task_status.as_deref(), Some("failed" | "verification_failed")) {
                    node.status = "failed".into();
                    node.note = format!("worker ended {}", node.task_status.as_deref().unwrap_or(""));
                } else {
                    node.status = "done".into();
                }
            }
            Err(_) => {
                node.status = "failed".into();
                node.note = format!("timed out after {WORKER_TIMEOUT}s");
            }
        }
        git(&wt, &["add", "-A"]).await;
        git(&wt, &["-c", "user.email=melon@local", "-c", "user.name=melon", "commit", "-qm", &format!("{}: {}", node.id, node.goal.chars().take(60).collect::<String>())]).await;
        let (_, names) = git(&wt, &["diff", "--name-only", base, "HEAD"]).await;
        node.changed = names.lines().filter(|n| !n.is_empty()).map(String::from).collect();
        if node.status == "done" && node.changed.is_empty() {
            // a worker that "finished" without touching anything almost always printed code instead of writing it
            node.status = "failed".into();
            node.note = format!("worker made no changes; it said: {:?}", last_text.chars().take(200).collect::<String>());
        }
        node.seconds = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
        say(json!({"kind": "done", "ok": node.status == "done"}));
    }

    async fn merge(&self, node: &mut Node) -> bool {
        let (code, out) = git(&self.int_dir, &["-c", "user.email=melon@local", "-c", "user.name=melon", "merge", "--no-ff", "-m", &format!("merge {}", node.id), &node.branch]).await;
        if code == 0 {
            return true;
        }
        let (_, conflicted) = git(&self.int_dir, &["diff", "--name-only", "--diff-filter=U"]).await;
        node.note = if conflicted.is_empty() { out.chars().rev().take(200).collect::<Vec<_>>().into_iter().rev().collect() } else { format!("merge conflict in: {}", conflicted.split_whitespace().collect::<Vec<_>>().join(", ")) };
        git(&self.int_dir, &["merge", "--abort"]).await;
        false
    }

    pub async fn run(mut self, request: &str, mut nodes: Vec<Node>, apply: bool) -> Result<Value, GraphError> {
        let t0 = Instant::now();
        let (code, head) = git(&self.repo, &["rev-parse", "HEAD"]).await;
        if code != 0 {
            return Err(GraphError("parallel mode needs a git repository with at least one commit".into()));
        }
        self.integration = format!("melon/{}/integration", self.run_id);
        let result = async {
            {
                let _g = self.git_lock.lock().await;
                git(&self.repo, &["worktree", "add", "-q", "-b", &self.integration, &self.int_dir.to_string_lossy(), &head]).await;
            }
            self.worktrees.lock().unwrap().push(self.int_dir.clone());
            for layer in waves(&nodes)? {
                let runnable: Vec<usize> = layer.iter().copied().filter(|i| nodes[*i].deps.iter().all(|d| nodes.iter().any(|n| n.id == *d && matches!(n.status.as_str(), "done" | "resolved")))).collect();
                for i in &layer {
                    if !runnable.contains(i) {
                        nodes[*i].status = "skipped".into();
                        nodes[*i].note = "a dependency did not complete".into();
                    }
                }
                let (_, base) = git(&self.int_dir, &["rev-parse", "HEAD"]).await;
                // workers run concurrently, each on its own copy of the node
                let mut copies: Vec<Node> = runnable.iter().map(|i| nodes[*i].clone()).collect();
                {
                    let this = &self;
                    let futs = copies.iter_mut().map(|n| this.run_worker(n, request, &base));
                    futures::future::join_all(futs).await;
                }
                for (k, i) in runnable.iter().enumerate() {
                    nodes[*i] = copies[k].clone();
                }
                // overlap report: same file touched by several workers in this layer
                let mut touched: BTreeMap<String, Vec<String>> = BTreeMap::new();
                for i in &runnable {
                    for f in &nodes[*i].changed {
                        touched.entry(f.clone()).or_default().push(nodes[*i].id.clone());
                    }
                }
                self.overlaps.extend(touched.into_iter().filter(|(_, ids)| ids.len() > 1));
                let mut conflicted = vec![];
                for i in &runnable {
                    // layer order == graph order, deterministic
                    if nodes[*i].status != "done" || nodes[*i].changed.is_empty() {
                        continue;
                    }
                    let mut n = nodes[*i].clone();
                    if !self.merge(&mut n).await {
                        n.status = "conflict".into();
                        conflicted.push(*i);
                    }
                    nodes[*i] = n;
                }
                for i in conflicted {
                    let mut n = nodes[i].clone();
                    self.rerun_on_integrated(&mut n, request).await;
                    nodes[i] = n;
                }
                self.combined_check(&nodes, &runnable).await;
            }
            let all_ok = nodes.iter().all(|n| matches!(n.status.as_str(), "done" | "resolved"));
            let applied = if apply && all_ok { self.apply_result().await } else { "not requested".into() };
            let status = if all_ok { "ok" } else if nodes.iter().any(|n| n.status == "conflict") { "conflict" } else { "partial" };
            Ok::<Value, GraphError>(json!({
                "status": status, "integration_branch": self.integration, "applied": applied,
                "wall_sec": (t0.elapsed().as_secs_f64() * 100.0).round() / 100.0,
                "sum_worker_sec": (nodes.iter().map(|n| n.seconds).sum::<f64>() * 100.0).round() / 100.0,
                "overlapping_files": self.overlaps, "combined_checks": self.combined_checks,
                "tasks": nodes.iter().map(|n| json!({"id": n.id, "goal": n.goal, "deps": n.deps, "files": n.files, "status": n.status, "branch": n.branch, "changed": n.changed, "seconds": n.seconds, "note": n.note, "task_status": n.task_status})).collect::<Vec<_>>(),
            }))
        }
        .await;
        self.cleanup().await;
        result
    }

    /// A merge conflict means two workers made incompatible edits. Re-run the loser on top of what is already integrated,
    /// with the full request, so the reconciliation follows the original intent.
    async fn rerun_on_integrated(&self, node: &mut Node, request: &str) {
        let (_, base) = git(&self.int_dir, &["rev-parse", "HEAD"]).await;
        let saved = node.note.clone();
        node.goal.push_str("\n\nNOTE: an earlier attempt at this task conflicted with changes from another worker that are now in your checkout. Reconcile with them so the ORIGINAL REQUEST is satisfied; do not blindly overwrite.");
        let orig_id = node.id.clone();
        node.id = format!("{orig_id}-retry");
        self.run_worker(node, request, &base).await;
        node.id = orig_id;
        if node.status == "done" && self.merge(node).await {
            node.status = "resolved".into();
            node.note = format!("re-run after conflict ({saved}); review recommended");
        } else {
            node.status = "conflict".into();
        }
    }

    async fn combined_check(&mut self, nodes: &[Node], ran: &[usize]) {
        let mut files: Vec<String> = ran.iter().flat_map(|i| nodes[*i].changed.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        files.sort();
        let mut cfg = self.config.clone();
        cfg.cwd = self.int_dir.clone();
        for (cmd, sub) in select_checks(&cfg, &files) {
            let r = run_check(&cmd, &self.int_dir.join(sub), &cfg).await;
            self.combined_checks.push(r.to_value());
        }
    }

    async fn apply_result(&self) -> String {
        let (_, dirty) = git(&self.repo, &["status", "--porcelain"]).await;
        if !dirty.is_empty() {
            return format!("skipped: working tree has uncommitted changes; merge branch {} yourself", self.integration);
        }
        let (code, out) = git(&self.repo, &["merge", "--ff-only", &self.integration]).await;
        if code == 0 { "fast-forwarded".into() } else { format!("skipped: {}", out.chars().rev().take(150).collect::<Vec<_>>().into_iter().rev().collect::<String>()) }
    }

    async fn cleanup(&self) {
        let _g = self.git_lock.lock().await;
        let wts = self.worktrees.lock().unwrap().clone();
        for wt in wts.iter().rev() {
            git(&self.repo, &["worktree", "remove", "--force", &wt.to_string_lossy()]).await;
        }
        let (_, branches) = git(&self.repo, &["branch", "--list", &format!("melon/{}/*", self.run_id)]).await;
        for b in branches.split_whitespace() {
            if b.ends_with("/integration") || b == "*" {
                continue;
            }
            git(&self.repo, &["branch", "-D", b]).await;
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

pub const PLAN_PROMPT: &str = "Split the request into tasks for parallel workers, each in a separate git checkout.
Reply with ONLY JSON: {\"tasks\":[{\"id\":\"short-id\",\"goal\":\"complete instruction\",\"deps\":[\"other-id\"],\"files\":[\"path\"]}]}
Rules: split ONLY if parts touch different files and can be done independently; otherwise return ONE task.
Use deps when one task needs another's output. Never invent files outside the request's scope. Max 6 tasks.";

/// Ask a model to decompose; any invalid answer degrades to one task.
pub async fn plan(client: &Arc<crate::client::llm_client::LLMClient>, request: &str) -> Vec<Node> {
    use futures::StreamExt;
    let mut text = String::new();
    let mut s = client.chat_completion(vec![json!({"role": "system", "content": PLAN_PROMPT}), json!({"role": "user", "content": request})], None, false, "plan");
    while let Some(ev) = s.next().await {
        if let crate::client::response::StreamEvent::MessageComplete { text: Some(t), .. } = ev {
            text.push_str(&t);
        }
    }
    let parsed = (|| {
        let raw = &text[text.find('{')?..=text.rfind('}')?];
        parse_graph(&serde_json::from_str::<Value>(raw).ok()?).ok()
    })();
    parsed.unwrap_or_else(|| vec![Node { id: "main".into(), goal: request.into(), deps: vec![], files: vec![], status: "pending".into(), branch: String::new(), changed: vec![], seconds: 0.0, note: String::new(), task_status: None }])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graph_rules() {
        let ok = parse_graph(&json!({"tasks": [{"id": "a", "goal": "x"}, {"id": "b", "goal": "y", "deps": ["a"]}]})).unwrap();
        assert_eq!(waves(&ok).unwrap(), vec![vec![0], vec![1]]);
        assert!(parse_graph(&json!({"tasks": []})).is_err());
        assert!(parse_graph(&json!({"tasks": [{"id": "a", "goal": "x"}, {"id": "a", "goal": "y"}]})).is_err());
        assert!(parse_graph(&json!({"tasks": [{"id": "a", "goal": "x", "deps": ["z"]}]})).is_err());
        assert!(parse_graph(&json!({"tasks": [{"id": "a", "goal": "x", "deps": ["b"]}, {"id": "b", "goal": "y", "deps": ["a"]}]})).unwrap_err().0.contains("cycle"));
    }
}
