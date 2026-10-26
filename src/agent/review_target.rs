//! What a review request points at: the working tree, the last N commits, one commit, the branch, or the PR checked out
//! here.
//!     "review my changes"          -> the working tree against HEAD
//!     "review my last 3 commits"   -> HEAD~3..HEAD
//!     "review commit 9fceb02"      -> 9fceb02^..9fceb02
//!     "review this branch / PR"    -> the merge base with main, through the working tree
//!     "review PR #42"              -> as the branch; when #42 is not checked out its head is fetched into a throwaway worktree
//! Anything it cannot resolve returns None and the normal single-agent review answers instead.
use crate::agent::review::git;
use crate::util::re::{lit_i, Re};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::process::Command;

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbb2f2ac";
const NUMBER_WORDS: &[(&str, usize)] = &[("a", 1), ("one", 1), ("two", 2), ("three", 3), ("four", 4), ("five", 5), ("six", 6), ("seven", 7), ("eight", 8), ("nine", 9), ("ten", 10)];

fn sha() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b[0-9a-f]{7,40}\b")) // git decides whether a hex-looking word ("defaced") is a commit
}
fn last_n() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(&format!(r"\b(?:last|latest|recent|past)\s+(\d+|{})?\s*commits?\b|\b(\d+)\s+commits?\b|\b(?:last|latest)\s+commit\b", NUMBER_WORDS.iter().map(|w| w.0).collect::<Vec<_>>().join("|"))))
}
fn pr_number() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"(?:\bpr\b|pull request|merge request)\s*#?(\d+)|(?:^|[^\w/])#(\d+)\b"))
}
fn branch_words() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit_i(r"\b(branch|pull request|pr|changes we made|whole change|everything we changed)\b"))
}

#[derive(Clone, Debug, Default)]
pub struct Target {
    /// "the last 3 commits"
    pub label: String,
    /// Arguments for `git diff`: ["HEAD"] is the working tree, [a, b] is a committed range.
    pub rev: Vec<String>,
    pub files: Vec<String>,
    /// Where the scope is not what the user asked for.
    pub notes: Vec<String>,
    /// "abc1234 subject" of each commit in the range.
    pub commits: Vec<String>,
    /// A throwaway worktree holding the PR head, when the PR is not the checked-out branch.
    pub cwd: Option<PathBuf>,
    /// The repo that worktree belongs to.
    pub repo: Option<PathBuf>,
}

impl Target {
    pub fn includes_worktree(&self) -> bool {
        self.rev.len() == 1
    }

    pub async fn cleanup(&self) {
        if let Some(c) = &self.cwd {
            git(self.repo.as_deref().unwrap_or(c), &["worktree", "remove", "--force", &c.to_string_lossy()]).await;
            let _ = std::fs::remove_dir_all(c);
        }
    }
}

async fn ok(cwd: &Path, args: &[&str]) -> String {
    git(cwd, args).await.trim().to_string()
}

async fn base_branch(cwd: &Path) -> String {
    for b in ["origin/main", "origin/master", "main", "master"] {
        if !ok(cwd, &["rev-parse", "--verify", "-q", b]).await.is_empty() {
            return b.into();
        }
    }
    String::new()
}

async fn files_of(cwd: &Path, rev: &[String], skip: &[&str]) -> Vec<String> {
    let mut a = vec!["diff", "--name-only"];
    a.extend(rev.iter().map(String::as_str));
    let mut names: Vec<String> = git(cwd, &a).await.split('\n').map(String::from).collect();
    if rev.len() == 1 {
        // a working-tree diff leaves out brand-new files git does not know yet
        names.extend(git(cwd, &["ls-files", "--others", "--exclude-standard"]).await.split('\n').map(String::from));
    }
    let mut out: Vec<String> = vec![];
    for n in names {
        if !n.is_empty() && !skip.iter().any(|s| n.ends_with(s)) && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// (head branch, base branch) of a PR from gh; the current branch's PR when no number is given.
async fn pr_info(cwd: &Path, number: &str) -> (String, String) {
    let mut args = vec!["pr", "view"];
    if !number.is_empty() {
        args.push(number);
    }
    args.extend(["--json", "headRefName,baseRefName", "-q", ".headRefName + \" \" + .baseRefName"]);
    let child = Command::new("gh").args(&args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn();
    let Ok(child) = child else { return (String::new(), String::new()) };
    let Ok(Ok(o)) = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output()).await else { return (String::new(), String::new()) };
    let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
    match out.split_once(' ') {
        Some((h, b)) => (h.into(), b.into()),
        None => (out, String::new()),
    }
}

async fn remote_ref(cwd: &Path, base: &str) -> String {
    for r in [format!("origin/{base}"), base.to_string()] {
        if !ok(cwd, &["rev-parse", "--verify", "-q", &r]).await.is_empty() {
            return r;
        }
    }
    String::new()
}

/// None when the request names something we cannot read from here (a PR that is not checked out, a commit that does not exist).
pub async fn resolve(cwd: &Path, message: &str, skip: &[&str]) -> Option<Target> {
    if let Some(c) = pr_number().captures(message) {
        let number = c.get(1).cloned().flatten().or(c.get(2).cloned().flatten()).unwrap_or_default();
        let (head, base) = pr_info(cwd, &number).await;
        if head.is_empty() {
            return None; // gh cannot say what this PR is
        }
        if head != ok(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).await {
            return remote_pr(cwd, &number, skip, &base).await;
        }
        return branch(cwd, &format!("PR #{number}"), skip, &base, vec![]).await;
    }
    let mut at = 0;
    while let Some((s, e)) = sha().find(&message[at..]) {
        let word = message[at + s..at + e].to_string();
        at += e.max(1);
        if ok(cwd, &["rev-parse", "--verify", "-q", &format!("{word}^{{commit}}")]).await.is_empty() {
            continue;
        }
        let rev = if ok(cwd, &["rev-parse", "--verify", "-q", &format!("{word}^")]).await.is_empty() { vec![EMPTY_TREE.to_string(), word.clone()] } else { vec![format!("{word}^"), word.clone()] };
        let files = files_of(cwd, &rev, skip).await;
        return (!files.is_empty()).then(|| Target { label: format!("commit {}", &word[..word.len().min(10)]), rev, files, ..Default::default() });
    }
    if let Some(c) = last_n().captures(message) {
        let raw = c.get(1).cloned().flatten().or(c.get(2).cloned().flatten()).unwrap_or_else(|| "1".into()).to_lowercase();
        let n = raw.parse::<usize>().unwrap_or_else(|_| NUMBER_WORDS.iter().find(|w| w.0 == raw).map_or(1, |w| w.1)).clamp(1, 20);
        if ok(cwd, &["rev-parse", "--verify", "-q", &format!("HEAD~{n}")]).await.is_empty() {
            return None;
        }
        let rev = vec![format!("HEAD~{n}"), "HEAD".to_string()];
        let files = files_of(cwd, &rev, skip).await;
        return (!files.is_empty()).then(|| Target { label: if n > 1 { format!("the last {n} commit(s)") } else { "the last commit".into() }, rev, files, ..Default::default() });
    }
    if branch_words().is_match(message) {
        let (_, base) = pr_info(cwd, "").await; // this branch's open PR names its real target
        let notes = if base.is_empty() { vec!["gh could not name this branch's PR base, so main/master is assumed".to_string()] } else { vec![] };
        return branch(cwd, "this branch", skip, &base, notes).await;
    }
    None
}

/// Bring origin/<base> up to date first. A stale local copy puts the merge base far back, and then the review reads every
/// commit that landed on the base since. Offline: carry on with what is there.
async fn refresh(cwd: &Path, r: &str) -> bool {
    match r.strip_prefix("origin/") {
        Some(b) => fetch(cwd, &[b]).await,
        None => true,
    }
}

async fn fetch(cwd: &Path, refspec: &[&str]) -> bool {
    let child = Command::new("git").args(["fetch", "-q", "origin"]).args(refspec).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).spawn();
    let Ok(mut child) = child else { return false };
    matches!(tokio::time::timeout(Duration::from_secs(40), child.wait()).await, Ok(Ok(s)) if s.success())
}

async fn commit_list(cwd: &Path, mb: &str, tip: &str) -> Vec<String> {
    git(cwd, &["log", "--format=%h %s", "--max-count=12", &format!("{mb}..{tip}")]).await.split('\n').filter(|l| !l.is_empty()).map(String::from).collect()
}

/// `pr_base` is the branch the PR merges into; without a PR it falls back to main/master. Every fallback is written into
/// `Target.notes`: a review of the wrong range that does not say so is worse than no review.
async fn branch(cwd: &Path, name: &str, skip: &[&str], pr_base: &str, mut notes: Vec<String>) -> Option<Target> {
    let mut base = if pr_base.is_empty() { String::new() } else { remote_ref(cwd, pr_base).await };
    if !pr_base.is_empty() && base.is_empty() {
        notes.push(format!("the PR's base branch {pr_base} is not here, so main/master is assumed"));
    }
    if base.is_empty() {
        base = base_branch(cwd).await;
    }
    if !refresh(cwd, &base).await {
        notes.push(format!("could not fetch {base}; its local copy may be stale and the range may include other people's commits"));
    }
    let mb = if base.is_empty() { String::new() } else { ok(cwd, &["merge-base", &base, "HEAD"]).await };
    if mb.is_empty() || mb == ok(cwd, &["rev-parse", "HEAD"]).await {
        return None; // on the base branch itself: the working tree is the only change there is
    }
    let rev = vec![mb.clone()];
    let files = files_of(cwd, &rev, skip).await;
    let commits = commit_list(cwd, &mb, "HEAD").await;
    (!files.is_empty()).then(|| Target { label: format!("{name} against {base} ({} commit(s) since {}, plus any uncommitted work)", commits.len(), &mb[..mb.len().min(8)]), rev, files, notes, commits, cwd: None, repo: None })
}

/// A PR whose branch is not checked out: fetch its head (GitHub keeps it at pull/<n>/head), check it out in a throwaway
/// worktree and review that.
async fn remote_pr(cwd: &Path, number: &str, skip: &[&str], pr_base: &str) -> Option<Target> {
    let r = format!("refs/melon/pr-{number}");
    if !fetch(cwd, &[&format!("+pull/{number}/head:{r}")]).await {
        return None;
    }
    let base = {
        let b = if pr_base.is_empty() { String::new() } else { remote_ref(cwd, pr_base).await };
        if b.is_empty() { base_branch(cwd).await } else { b }
    };
    let mut notes = if !pr_base.is_empty() && base.ends_with(pr_base) {
        vec![]
    } else {
        vec![format!("the PR's base {} is not here, so {} is assumed", if pr_base.is_empty() { "(unknown)" } else { pr_base }, if base.is_empty() { "nothing" } else { &base })]
    };
    if !refresh(cwd, &base).await {
        notes.push(format!("could not fetch {base}; its local copy may be stale and the range may include other people's commits"));
    }
    let mb = if base.is_empty() { String::new() } else { ok(cwd, &["merge-base", &base, &r]).await };
    if mb.is_empty() {
        return None;
    }
    let tree = tempfile::Builder::new().prefix(&format!("melon-pr-{number}-")).tempdir().ok()?.keep();
    if ok(cwd, &["worktree", "add", "--detach", &tree.to_string_lossy(), &r]).await.is_empty() && !tree.join(".git").exists() {
        let _ = std::fs::remove_dir_all(&tree);
        return None;
    }
    let rev = vec![mb.clone()];
    let files = files_of(&tree, &rev, skip).await;
    let commits = commit_list(cwd, &mb, &r).await;
    let t = Target { label: format!("PR #{number} against {base} ({} commit(s) since {})", commits.len(), &mb[..mb.len().min(8)]), rev, files, notes, commits, cwd: Some(tree), repo: Some(cwd.to_path_buf()) };
    if t.files.is_empty() {
        t.cleanup().await;
        return None;
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patterns() {
        assert!(last_n().is_match("review my last 3 commits"));
        assert!(last_n().is_match("review the latest commit"));
        assert!(pr_number().is_match("review PR #42"));
        assert!(pr_number().is_match("review #7 please"));
        assert!(branch_words().is_match("review this branch"));
        assert!(!branch_words().is_match("review my changes"));
    }
}
