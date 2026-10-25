use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::re::lit;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const MAX_CHARS: usize = 6000;
/// A review reads the whole change in one call, big files are cut so none crowd out the rest.
const PATCH_TOTAL: usize = 24_000;
const PATCH_PER_FILE: usize = 3_500;

pub struct GitTool;

async fn git(cwd: &Path, argv: &[String]) -> Result<(i32, String, String), String> {
    let mut full = vec!["--no-pager".to_string()];
    full.extend(argv.iter().cloned());
    let out = tokio::time::timeout(Duration::from_secs(30), Command::new("git").args(&full).current_dir(cwd).kill_on_drop(true).output()).await.map_err(|_| "git timed out".to_string())?.map_err(|e| e.to_string())?;
    Ok((out.status.code().unwrap_or(1), String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string()))
}

async fn git_quiet(cwd: &Path, argv: &[&str]) -> String {
    let a: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    match git(cwd, &a).await {
        Ok((0, o, _)) => o.trim().to_string(),
        _ => String::new(),
    }
}

/// Everything a review needs to fix its scope once: where we are, what is uncommitted, and which commits and files the
/// branch adds over its base.
async fn scope(cwd: &Path, base: Option<String>) -> ToolResult {
    if git_quiet(cwd, &["rev-parse", "--git-dir"]).await.is_empty() {
        return ToolResult::error("not a git repository");
    }
    let mut base = base;
    if base.is_none() {
        for cand in ["origin/main", "origin/master", "main", "master"] {
            if !git_quiet(cwd, &["rev-parse", "--verify", "-q", cand]).await.is_empty() {
                base = Some(cand.to_string());
                break;
            }
        }
    }
    let head = git_quiet(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
    let or = |s: String, d: &str| if s.is_empty() { d.to_string() } else { s };
    let mut parts = vec![
        format!("branch: {head}"),
        format!("status:\n{}", or(git_quiet(cwd, &["status", "--short"]).await, "(clean)")),
        format!("uncommitted (staged+unstaged) vs HEAD:\n{}", or(git_quiet(cwd, &["diff", "--stat", "HEAD"]).await, "(none)")),
    ];
    match &base {
        Some(b) if head != b.rsplit('/').next().unwrap_or("") => {
            let commits = git_quiet(cwd, &["log", "--oneline", &format!("{b}..HEAD"), "-n", "15"]).await;
            parts.push(format!("branch commits not in {b}:\n{}", or(commits.clone(), "(none)")));
            if !commits.is_empty() {
                parts.push(format!("branch files vs {b}:\n{}", git_quiet(cwd, &["diff", "--stat", &format!("{b}...HEAD")]).await));
            }
        }
        _ => parts.push("base: none found, or already on it; only uncommitted changes are in scope".into()),
    }
    ToolResult::ok(crate::util::text::head_chars(&parts.join("\n\n"), MAX_CHARS))
}

/// The whole change as one readable patch. Example: 6 changed files cost one call, not six.
async fn patch(cwd: &Path, rev: Option<&str>, path: Option<&str>) -> ToolResult {
    let mut argv: Vec<String> = vec!["diff".into(), "--no-color".into(), "-U2".into(), rev.unwrap_or("HEAD").into()];
    if let Some(p) = path {
        argv.push("--".into());
        argv.push(p.into());
    }
    let (code, out, err) = match git(cwd, &argv).await {
        Ok(x) => x,
        Err(e) => return ToolResult::error(e),
    };
    if code != 0 {
        let e = crate::util::text::head_chars(err.trim(), 500);
        return ToolResult::error(if e.is_empty() { "git failed".into() } else { e });
    }
    // split before each "diff --git " line
    let mut files: Vec<String> = vec![];
    for line in out.split_inclusive('\n') {
        if line.starts_with("diff --git ") || files.is_empty() {
            files.push(String::new());
        }
        files.last_mut().unwrap().push_str(line);
    }
    files.retain(|f| !f.trim().is_empty());
    if files.is_empty() {
        return ToolResult::ok("(no changes; new untracked files are not in a diff, read them directly)");
    }
    let (mut shown, mut used, mut cut, mut skipped) = (String::new(), 0usize, vec![], vec![]);
    for f in files {
        let name = f.lines().next().unwrap_or("").rsplit(" b/").next().unwrap_or("").to_string();
        if used >= PATCH_TOTAL {
            skipped.push(name);
            continue;
        }
        let f = if f.chars().count() > PATCH_PER_FILE {
            cut.push(name);
            format!("{}\n... [file cut here]\n", f.chars().take(PATCH_PER_FILE).collect::<String>())
        } else {
            f
        };
        used += f.chars().count();
        shown.push_str(&f);
    }
    let mut note = String::new();
    if !cut.is_empty() {
        note.push_str(&format!("\ncut (read the rest with path=<file>): {}", cut.join(", ")));
    }
    if !skipped.is_empty() {
        note.push_str(&format!("\nnot shown, over the size limit: {}", skipped.join(", ")));
    }
    ToolResult::ok(format!("{}{note}", shown.trim_end()))
}

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }
    fn description(&self) -> String {
        "Read-only git history for finding what changed and why: status, diff, log, show, blame, find_change. Needs no approval. To add, commit, switch branches, push or pull, run git in the shell tool (`git commit -m ...`).".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn schema(&self) -> Value {
        schema_of(
            json!({
                "op": {"type": "string", "enum": ["scope", "patch", "status", "diff", "log", "show", "blame", "find_change"], "description": "scope: one-call review snapshot (branch, status, base range, stat of working-tree and branch changes). patch: the actual changes, every changed file in ONE call (uncommitted vs HEAD, or vs `rev`); use it instead of one diff call per file. status: changed files. diff: stat of working tree (or of `path`, full). log: recent commits touching `path`. show: one commit's files (and its diff of `path`). blame: who last changed lines `lines` of `path`. find_change: commits that added/removed the text `query` (git log -S), the fastest way to find when a behaviour changed."},
                "path": {"type": ["string", "null"], "description": "File to focus on (relative to the working directory)"},
                "rev": {"type": ["string", "null"], "description": "Commit, branch or range, e.g. HEAD~3 or main..HEAD"},
                "lines": {"type": ["string", "null"], "description": "blame only: 'start,end' line range, e.g. '40,80'"},
                "query": {"type": ["string", "null"], "description": "find_change only: text to look for"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 30, "default": 8, "description": "Max commits for log/find_change"},
            }),
            &["op"],
        )
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let (op, path, rev, lines, query) = (inv.str("op").unwrap_or(""), inv.str("path").filter(|s| !s.is_empty()), inv.str("rev").filter(|s| !s.is_empty()), inv.str("lines").filter(|s| !s.is_empty()), inv.str("query").filter(|s| !s.is_empty()));
        let limit = inv.params["limit"].as_u64().unwrap_or(8);
        for (label, v) in [("rev", rev), ("path", path), ("query", query)] {
            if v.map_or(false, |v| v.starts_with('-')) {
                return ToolResult::error(format!("{label} may not start with '-'"));
            }
        }
        if let Some(r) = rev {
            if !lit(r"^[\w./~^@{}:-]+$").is_match(r) {
                return ToolResult::error(format!("Unusual revision '{r}'"));
            }
        }
        let tail: Vec<String> = path.map(|p| vec!["--".into(), p.into()]).unwrap_or_default();
        let pretty = "--pretty=format:%h %ad %an: %s".to_string();
        let s = |x: &str| x.to_string();
        let argv: Vec<String> = match op {
            "scope" => return scope(&inv.cwd, rev.map(String::from)).await,
            "patch" => return patch(&inv.cwd, rev, path).await,
            "status" => vec![s("status"), s("--short"), s("--branch")],
            "diff" => [vec![s("diff")], rev.map(String::from).into_iter().collect(), if path.is_some() { vec![] } else { vec![s("--stat")] }, tail].concat(),
            "log" => [vec![s("log"), format!("-n{limit}"), s("--date=short"), pretty], rev.map(String::from).into_iter().collect(), tail].concat(),
            "show" => {
                let Some(r) = rev else { return ToolResult::error("show needs rev") };
                [vec![s("show"), s("--date=short"), pretty], if path.is_some() { vec![] } else { vec![s("--stat")] }, vec![r.to_string()], tail].concat()
            }
            "blame" => {
                let (Some(p), Some(l)) = (path, lines.filter(|l| lit(r"^\d+,\d+$").is_match(l))) else { return ToolResult::error("blame needs path and lines like '40,80'") };
                [vec![s("blame"), s("-s"), s("-w"), s("-L"), l.to_string()], rev.map(String::from).into_iter().collect(), vec![s("--"), p.to_string()]].concat()
            }
            _ => {
                let Some(q) = query else { return ToolResult::error("find_change needs query") };
                [vec![s("log"), format!("-n{limit}"), s("--date=short"), pretty, s("--stat"), s("-S"), q.to_string()], tail].concat()
            }
        };
        let (code, out, err) = match git(&inv.cwd, &argv).await {
            Ok(x) => x,
            Err(e) => return ToolResult::error(e),
        };
        if code != 0 {
            let e = crate::util::text::head_chars(err.trim(), 500);
            return ToolResult::error(if e.is_empty() { "git failed".into() } else { e });
        }
        let mut text = out.trim().to_string();
        if text.is_empty() {
            text = "(no output)".into();
        }
        if text.chars().count() > MAX_CHARS {
            text = format!("{}\n... [clipped at {MAX_CHARS} chars; narrow with path/rev/limit]", text.chars().take(MAX_CHARS).collect::<String>());
        }
        ToolResult::ok(text)
    }
}
