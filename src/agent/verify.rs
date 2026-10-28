//! Change detection and the verification commands run after an agent edits files.
use crate::config::Config;
use crate::tools::builtin::shell::build_environment;
use crate::util::pyscan;
use crate::util::re::{lit, Re};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::process::Command;

const MAX_HASH_BYTES: u64 = 5_000_000;
const OUTPUT_TAIL: usize = 4000;
const MAX_CHECK_FILES: usize = 100; // a check command naming thousands of files is a bulk move or a build dir, not an edit
const MAX_COMMAND_SHOWN: usize = 300; // a failing command is echoed back to the model; a long file list must not ride along
// a change to these can't break a typecheck or a linter, so a rule's checks are skipped when only these changed
const STATIC_SUFFIX: &[&str] = &["html", "htm", "md", "txt", "svg", "png", "jpg", "jpeg", "gif", "ico", "webp"];

/// Build output and dependency dirs: never something the agent meant to edit, even when git does not ignore them
/// (`mv .next .next.stale` turned 7,600 cache files into "your changes" and fed them to a linter).
fn generated() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"(^|/)(node_modules|\.next[^/]*|\.nuxt|\.turbo|\.cache|dist|build|target|coverage|__pycache__|\.venv|venv)/"))
}

async fn git(cwd: &Path, args: &[&str]) -> (i32, Vec<u8>) {
    match Command::new("git").args(args).current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::null()).output().await {
        Ok(o) => (o.status.code().unwrap_or(1), o.stdout),
        Err(_) => (1, vec![]),
    }
}

pub type Snapshot = BTreeMap<String, String>;

/// path -> content hash for every dirty/untracked file, or None outside git. Comparing two snapshots isolates what
/// *this run* changed from edits the user already had in flight.
pub async fn snapshot(cwd: &Path) -> Option<Snapshot> {
    let (code, out) = git(cwd, &["status", "--porcelain=v1", "-z", "-uall"]).await;
    if code != 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&out).to_string();
    let entries: Vec<&str> = text.split('\0').collect();
    let mut snap = Snapshot::new();
    let mut i = 0;
    while i < entries.len() {
        let e = entries[i];
        i += 1;
        if e.len() < 4 {
            continue;
        }
        let (status, path) = (&e[..2], &e[3..]);
        if status.starts_with('R') || status.starts_with('C') {
            i += 1; // rename/copy entries carry the old path next
        }
        let f = cwd.join(path);
        let h = match std::fs::metadata(&f) {
            Ok(m) if m.is_file() && m.len() <= MAX_HASH_BYTES => std::fs::read(&f).map(|b| hex::encode(Sha1::digest(&b))).unwrap_or_else(|_| "unreadable".into()),
            Ok(_) => "large".into(),
            Err(_) => "deleted".into(),
        };
        snap.insert(path.to_string(), h);
    }
    Some(snap)
}

/// A file that went from dirty to clean is not listed: after "commit this" every committed file looks reverted.
pub fn changed_since(before: Option<&Snapshot>, after: Option<&Snapshot>) -> Vec<String> {
    let (Some(b), Some(a)) = (before, after) else { return vec![] };
    a.iter().filter(|(p, h)| b.get(*p) != Some(h) && !generated().is_match(p)).map(|(p, _)| p.clone()).collect()
}

const JS_SUFFIX: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];

pub fn package_runner(cwd: &Path, pkg_dir: &Path) -> String {
    for d in pkg_dir.ancestors() {
        for (lock, runner) in [("pnpm-lock.yaml", "pnpm"), ("yarn.lock", "yarn"), ("bun.lockb", "bun"), ("bun.lock", "bun"), ("package-lock.json", "npm")] {
            if d.join(lock).exists() {
                return runner.into();
            }
        }
        if d == cwd {
            break;
        }
    }
    "npm".into()
}

fn quote(s: &str) -> String {
    shell_words::quote(s).to_string()
}

/// The test file for a changed module: itself if it is one, else test_<name>.py next to it or under tests/.
fn py_tests(cwd: &Path, f: &str) -> Vec<String> {
    let p = Path::new(f);
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let stem = p.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if name.starts_with("test_") || stem.ends_with("_test") {
        return vec![f.to_string()];
    }
    let mut found: Vec<String> = vec![p.parent().unwrap_or(Path::new("")).join(format!("test_{name}")).to_string_lossy().to_string()];
    if cwd.join("tests").is_dir() {
        for e in walkdir::WalkDir::new(cwd.join("tests")).into_iter().flatten() {
            if e.file_name().to_string_lossy() == format!("test_{name}") {
                found.push(e.path().strip_prefix(cwd).unwrap_or(e.path()).to_string_lossy().to_string());
            }
        }
    }
    found.into_iter().filter(|t| cwd.join(t).is_file()).collect::<std::collections::BTreeSet<_>>().into_iter().collect()
}

/// eslint on just the changed files, per package, errors only. Nothing when eslint is not installed.
fn eslint(cwd: &Path, files: &[String]) -> Vec<(String, String)> {
    let mut by_pkg: BTreeMap<std::path::PathBuf, Vec<String>> = BTreeMap::new();
    for f in files {
        let mut d = cwd.join(f).parent().unwrap().to_path_buf();
        while !d.join("package.json").is_file() && d != cwd && d.starts_with(cwd) {
            d = d.parent().unwrap().to_path_buf();
        }
        by_pkg.entry(d.clone()).or_default().push(crate::util::paths::rel_display(&cwd.join(f), &d));
    }
    let mut out = vec![];
    for (d, rel) in by_pkg {
        if [&d, &PathBuf::from(cwd)].iter().any(|x| x.join("node_modules/.bin/eslint").exists()) {
            out.push((format!("{} exec eslint --quiet {}", package_runner(cwd, &d), rel.iter().map(|r| quote(r)).collect::<Vec<_>>().join(" ")), {
                let r = crate::util::paths::rel_display(&d, cwd);
                if r.is_empty() || r == d.to_string_lossy() { ".".into() } else { r }
            }));
        }
    }
    out
}

use std::path::PathBuf;

/// One quick check of the files this run changed, never a build, the whole suite or a project-wide typecheck.
pub fn detect_checks(cwd: &Path, changed: &[String]) -> Vec<(String, String)> {
    let cwd = crate::util::paths::resolve(cwd);
    let live: Vec<&String> = changed.iter().filter(|f| cwd.join(f).is_file()).collect();
    let mut checks: Vec<(String, String)> = vec![];
    let py: Vec<&&String> = live.iter().filter(|f| f.ends_with(".py")).collect();
    if !py.is_empty() {
        let tests: Vec<String> = py.iter().flat_map(|f| py_tests(&cwd, f)).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        checks.push((
            if tests.is_empty() {
                format!("python3 -m py_compile {}", py.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" "))
            } else {
                format!("python3 -m pytest -q -x {}", tests.iter().map(|t| quote(t)).collect::<Vec<_>>().join(" "))
            },
            ".".into(),
        ));
    }
    let go: Vec<String> = live.iter().filter(|f| f.ends_with(".go")).map(|f| format!("./{}", Path::new(f.as_str()).parent().unwrap().to_string_lossy())).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    if !go.is_empty() && cwd.join("go.mod").exists() {
        checks.push((format!("go vet {}", go.join(" ")), ".".into()));
    }
    if live.iter().any(|f| f.ends_with(".rs")) && cwd.join("Cargo.toml").exists() {
        checks.push(("cargo check -q".into(), ".".into())); // rust cannot check a single file; check is still far short of a test run
    }
    let js: Vec<String> = live.iter().filter(|f| Path::new(f.as_str()).extension().map_or(false, |e| JS_SUFFIX.contains(&e.to_string_lossy().as_ref()))).map(|f| f.to_string()).collect();
    checks.extend(eslint(&cwd, &js));
    checks
}

/// Specs (under the rule's cwd, matching tests_glob) that import one of the changed files.
pub fn related_tests(root: &Path, rule: &crate::config::VerifyRule, hits: &[String]) -> Vec<String> {
    let Some(g) = &rule.tests_glob else { return vec![] };
    let base = root.join(&rule.cwd);
    let stems: Vec<String> = hits.iter().filter(|f| !lit(r"\.(spec|test)\.").is_match(f)).filter_map(|f| Path::new(f).file_stem().map(|s| s.to_string_lossy().to_string())).collect();
    let mut found = vec![];
    for e in walkdir::WalkDir::new(&base).into_iter().flatten().filter(|e| e.file_type().is_file()) {
        let rel = e.path().strip_prefix(&base).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
        if !crate::util::search::glob_match(&rel, g) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
        if stems.iter().any(|st| Re::new(&format!(r#"/{}["']"#, regex::escape(st)), false).map_or(false, |r| r.is_match(&text))) {
            found.push(rel);
        }
    }
    found.sort();
    found.truncate(8);
    found
}

/// (command, cwd) pairs to run for this change set. Explicit commands win, then rules matching the changed paths
/// (monorepos), then auto-detection.
pub fn select_checks(config: &Config, changed: &[String]) -> Vec<(String, String)> {
    let mut checks: Vec<(String, String)> = config.verify_commands.iter().map(|c| (c.clone(), ".".into())).collect();
    for rule in &config.verify_rules {
        let hits: Vec<String> = changed.iter().filter(|f| rule.paths.iter().any(|p| f.starts_with(p.as_str()))).cloned().collect();
        let only_static = hits.iter().all(|f| Path::new(f).extension().map_or(false, |e| STATIC_SUFFIX.contains(&e.to_string_lossy().to_lowercase().as_str())));
        if hits.is_empty() || only_static {
            continue;
        }
        let rel: Vec<String> = hits.iter().take(MAX_CHECK_FILES).map(|f| rel_from(f, &rule.cwd)).collect();
        let files = rel.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" ");
        let tests_l = related_tests(&config.cwd, rule, &hits);
        let tests = tests_l.iter().map(|t| quote(t)).collect::<Vec<_>>().join(" ");
        for cmd in &rule.commands {
            if cmd.contains("{tests}") && tests.is_empty() {
                continue;
            }
            let item = (cmd.replace("{files}", &files).replace("{tests}", &tests), rule.cwd.clone());
            if !checks.contains(&item) {
                checks.push(item);
            }
        }
    }
    if checks.is_empty() && config.auto_verify {
        checks = detect_checks(&config.cwd, changed);
    }
    checks
}

/// `os.path.relpath(file, start)` for repo-relative strings.
fn rel_from(file: &str, start: &str) -> String {
    let f: Vec<&str> = file.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let s: Vec<&str> = start.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let common = f.iter().zip(s.iter()).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = vec![".."; s.len() - common];
    parts.extend(&f[common..]);
    if parts.is_empty() { ".".into() } else { parts.join("/") }
}

#[derive(Clone, Debug)]
pub struct CheckResult {
    pub command: String,
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub seconds: f64,
    pub output_tail: String,
}

impl CheckResult {
    pub fn to_value(&self) -> Value {
        json!({"command": self.command, "ok": self.ok, "exit_code": self.exit_code, "seconds": (self.seconds * 100.0).round() / 100.0})
    }
}

pub async fn run_check(command: &str, cwd: &Path, config: &Config) -> CheckResult {
    let t = Instant::now();
    let argv = crate::safety::sandbox::wrap(vec!["/bin/bash".into(), "-c".into(), command.into()], config);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(cwd).env_clear().envs(build_environment(config)).stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null()).kill_on_drop(true);
    let spawn = crate::util::detach::detach(&mut cmd).spawn();
    let child = match spawn {
        Ok(c) => c,
        Err(e) => return CheckResult { command: command.into(), ok: false, exit_code: None, seconds: 0.0, output_tail: e.to_string() },
    };
    let pid = child.id();
    let timeout = Duration::from_secs(config.verify_timeout_sec);
    struct Kill(Option<u32>);
    impl Drop for Kill {
        fn drop(&mut self) {
            // the user steered mid-check: a typecheck of the old diff is not worth finishing, and must not linger
            if let Some(p) = self.0 {
                unsafe { libc::killpg(p as i32, libc::SIGKILL) };
            }
        }
    }
    let mut guard = Kill(pid);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => {
            guard.0 = None;
            let mut text = String::from_utf8_lossy(&o.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            let tail: String = { let c: Vec<char> = text.chars().collect(); c[c.len().saturating_sub(OUTPUT_TAIL)..].iter().collect() };
            CheckResult { command: command.into(), ok: o.status.code() == Some(0), exit_code: o.status.code(), seconds: t.elapsed().as_secs_f64(), output_tail: tail }
        }
        _ => CheckResult { command: command.into(), ok: false, exit_code: None, seconds: t.elapsed().as_secs_f64(), output_tail: format!("timed out after {}s", config.verify_timeout_sec) },
    }
}

/// Lines that name an error, with line and column numbers dropped so an unrelated edit above them doesn't make an old
/// error look new.
fn error_lines(text: &str) -> HashSet<String> {
    static R: OnceLock<Re> = OnceLock::new();
    let num = R.get_or_init(|| lit(r"\(\d+,\d+\)|:\d+(:\d+)?"));
    text.lines().filter(|l| l.to_lowercase().contains("error")).map(|l| num.replace_all(l.trim(), "").to_string()).collect()
}

/// Puts the agent's file contents back when dropped, so a cancelled or crashed baseline run never leaves the tree reverted.
/// The modified time goes back too: the agent tracks it to notice edits made behind its back, and a rewrite would look like one.
struct Restore(Vec<(PathBuf, Option<(Vec<u8>, Option<std::time::SystemTime>)>)>);
impl Restore {
    fn of(cwd: &Path, files: &[String]) -> Self {
        Restore(files.iter().map(|f| {
            let p = cwd.join(f);
            let saved = std::fs::read(&p).ok().map(|b| (b, std::fs::metadata(&p).and_then(|m| m.modified()).ok()));
            (p, saved)
        }).collect())
    }
}
impl Drop for Restore {
    fn drop(&mut self) {
        for (p, saved) in &self.0 {
            match saved {
                Some((b, mtime)) => {
                    if std::fs::write(p, b).is_ok() {
                        if let (Some(t), Ok(f)) = (mtime, std::fs::File::options().write(true).open(p)) {
                            let _ = f.set_modified(*t);
                        }
                    }
                }
                None => { let _ = std::fs::remove_file(p); }
            }
        }
    }
}

/// Was this check already failing before the run touched anything? The run's edits are swapped out for the pre-run copies
/// (`backup_ref`), the failing commands run once more, and the agent's files go back. A failure whose errors all appear in
/// that clean run is not the agent's to fix (a stale `.next` types file once sent a restyle of one HTML page into a "repair").
/// `cache` keeps the clean-tree result per command, since it can't change while the task runs.
/// Returns (failures that are new, failures that were already there).
pub async fn split_preexisting(
    cwd: &Path,
    backup_ref: &str,
    changed: &[String],
    created: &[String],
    failing: Vec<(CheckResult, PathBuf)>,
    cache: &mut BTreeMap<String, CheckResult>,
    config: &Config,
) -> (Vec<CheckResult>, Vec<CheckResult>) {
    if failing.iter().any(|(r, _)| !cache.contains_key(&r.command)) {
        let _restore = Restore::of(cwd, changed);
        undo(cwd, backup_ref, changed, created).await;
        for (r, dir) in &failing {
            if !cache.contains_key(&r.command) {
                cache.insert(r.command.clone(), run_check(&r.command, dir, config).await);
            }
        }
    } // `_restore` drops here and puts the agent's files back
    let (mut new, mut old) = (vec![], vec![]);
    for (r, _) in failing {
        let base = &cache[&r.command];
        let (now, then) = (error_lines(&r.output_tail), error_lines(&base.output_tail));
        // output that names a file the run changed is about the run's work (a failing test it was asked to fix fails the same way
        // before and after), and output with no error lines says nothing either way: both stay the agent's to repair
        let names_change = changed.iter().any(|f| r.output_tail.contains(f.as_str()) || Path::new(f).file_name().map_or(false, |n| r.output_tail.contains(n.to_string_lossy().as_ref())));
        if !base.ok && !now.is_empty() && now.is_subset(&then) && !names_change { old.push(r) } else { new.push(r) }
    }
    (new, old)
}

pub fn repair_prompt(failures: &[CheckResult], files: &[String]) -> String {
    let blocks = failures.iter().map(|f| format!("$ {}\n(exit {})\n{}", f.command.chars().take(MAX_COMMAND_SHOWN).collect::<String>(), f.exit_code.map_or("none".into(), |c| c.to_string()), f.output_tail)).collect::<Vec<_>>().join("\n\n");
    format!(
        "Automated verification FAILED after your changes (files touched: {}).\n\n{blocks}\n\nFind the root cause and make the smallest fix. Do not weaken or delete a test just to make it pass; only change a test if its expectation is genuinely wrong, and say so. If the failure is in files you did not touch, it was already broken: say so and stop. Never move or delete build or cache directories to make a check pass. Stop when the checks should pass.",
        files.iter().take(20).cloned().collect::<Vec<_>>().join(", ")
    )
}

/// A commit holding the working tree as it is right now (tracked files, staged or not), made without touching the
/// tree, index or stash list. Clean tree: HEAD. None outside git or in a repo with no commits.
pub async fn backup(cwd: &Path) -> Option<String> {
    let (code, out) = git(cwd, &["stash", "create"]).await;
    let mut sha = if code == 0 { String::from_utf8_lossy(&out).trim().to_string() } else { String::new() };
    if sha.is_empty() {
        let (code, out) = git(cwd, &["rev-parse", "--verify", "HEAD"]).await;
        sha = if code == 0 { String::from_utf8_lossy(&out).trim().to_string() } else { String::new() };
    }
    if !sha.is_empty() {
        git(cwd, &["update-ref", "refs/melon/backup", &sha]).await; // keep the objects from being garbage collected while the user may still undo
        return Some(sha);
    }
    None
}

/// Put back what a run changed: (restored, deleted, skipped). Files the run created are deleted; files that existed
/// untracked before the run have no copy in `ref`, so they are reported, not touched.
pub async fn undo(cwd: &Path, r: &str, changed: &[String], created: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut restored, mut deleted, mut skipped) = (vec![], vec![], vec![]);
    for f in changed {
        let (code, _) = git(cwd, &["cat-file", "-e", &format!("{r}:{f}")]).await;
        if code == 0 {
            let (code, _) = git(cwd, &["restore", &format!("--source={r}"), "--worktree", "--", f]).await;
            if code == 0 { restored.push(f.clone()) } else { skipped.push(f.clone()) }
        } else if created.contains(f) && cwd.join(f).is_file() {
            let _ = std::fs::remove_file(cwd.join(f));
            deleted.push(f.clone());
        } else {
            skipped.push(f.clone());
        }
    }
    (restored, deleted, skipped)
}

// --- signature guard ---------------------------------------------------------------------------------------------
// Checks prove the code runs; they can't see a caller that isn't in the repo or in the tests the agent knows of. The
// benchmark case: asked to fix `def unique_by(items, key, seen=set())`, the agent dropped `seen` altogether, its own
// tests passed, and every caller that passed seen= got a TypeError. These helpers list such breaks so the agent is asked
// once to keep old calls working (or to say why it can't).

fn test_file() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"(^|/)(tests?|__tests__)/|(^|/)test_[^/]*\.py$|_test\.py$|\.(test|spec)\.[jt]sx?$"))
}

fn py_signatures(src: &str) -> Vec<(String, pyscan::Sig)> {
    pyscan::defs(src)
        .into_iter()
        .filter(|d| !d.is_class && (!d.name.starts_with('_') || d.name == "__init__") && d.parents.iter().all(|p| !p.starts_with('_')))
        .map(|d| (if d.parents.is_empty() { d.name.clone() } else { format!("{}.{}", d.parents.join("."), d.name) }, pyscan::parse_sig(&d.params)))
        .collect()
}

/// ([(what broke, the name it is about)], names still defined) for one Python file.
fn py_breaks(old_src: &str, new_src: &str) -> (Vec<(String, String)>, HashSet<String>) {
    if !pyscan::looks_balanced(old_src) || !pyscan::looks_balanced(new_src) {
        return (vec![], HashSet::new());
    }
    let (old, new) = (py_signatures(old_src), py_signatures(new_src));
    let mut out = vec![];
    for (name, a) in &old {
        let Some((_, b)) = new.iter().find(|(n, _)| n == name) else {
            out.push((format!("{name}() was removed or renamed"), name.rsplit('.').next().unwrap().to_string()));
            continue;
        };
        let old_all: Vec<&String> = a.positional.iter().chain(a.kwonly.iter()).collect();
        let new_all: Vec<&String> = b.positional.iter().chain(b.kwonly.iter()).collect();
        if b.kwarg.is_none() {
            for p in &old_all {
                if !new_all.contains(p) {
                    out.push((format!("{name}() no longer accepts `{p}`"), (*p).clone()));
                }
            }
        }
        let required: Vec<&String> = b.positional[..b.positional.len() - b.positional_defaults.min(b.positional.len())].iter().chain(b.kwonly_required.iter()).collect();
        for p in required {
            if !old_all.contains(&p) {
                out.push((format!("{name}() now requires `{p}`"), p.clone()));
            }
        }
        let moved: Vec<&String> = a.positional.iter().enumerate().filter(|(i, p)| b.positional.contains(p) && b.positional.iter().position(|x| x == *p) != Some(*i) && !matches!(p.as_str(), "self" | "cls")).map(|(_, p)| p).collect();
        if let Some(m) = moved.first() {
            out.push((format!("{name}() reorders its positional parameters ({} -> {})", a.positional.join(", "), b.positional.join(", ")), (*m).clone()));
        }
        if let (Some(v), None) = (&a.vararg, &b.vararg) {
            out.push((format!("{name}() no longer accepts *{v}"), v.clone()));
        }
        if let (Some(k), None) = (&a.kwarg, &b.kwarg) {
            out.push((format!("{name}() no longer accepts **{k}"), k.clone()));
        }
    }
    let names = new.iter().map(|(n, _)| n.rsplit('.').next().unwrap().to_string()).collect();
    (out, names)
}

fn js_export() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"export\s+(?:default\s+)?(?:async\s+)?function\s*\*?\s*(\w+)\s*(?:<[^>()]*>)?\s*\(|export\s+const\s+(\w+)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\s*\w*\s*)?(?:<[^>()]*>)?\s*\("))
}

/// Exported functions -> [(parameter text, optional)]. A light reader, not a TS parser.
fn js_signatures(src: &str) -> Vec<(String, Vec<(String, bool)>)> {
    let mut sigs = vec![];
    let re = js_export();
    let mut at = 0;
    while at < src.len() {
        let Some(c) = re.captures(&src[at..]) else { break };
        let (_, e) = re.find(&src[at..]).unwrap();
        let name = c[1].clone().or(c[2].clone()).unwrap_or_default();
        let start = at + e;
        let (mut depth, mut i) = (1i32, start);
        let b = src.as_bytes();
        while i < b.len() && depth > 0 {
            match b[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        let inner = &src[start..i.saturating_sub(1).max(start)];
        let (mut params, mut cur, mut d) = (vec![], String::new(), 0i32);
        for ch in inner.chars().chain(std::iter::once(',')) {
            if ch == ',' && d == 0 {
                let p = cur.trim().to_string();
                if !p.is_empty() {
                    let top = lit(r"[\[{(<].*").replace(&p, "").to_string(); // what sits before any nested type or pattern
                    let optional = p.starts_with("...") || top.contains('?') || p.replace("=>", "").contains('=');
                    params.push((p, optional));
                }
                cur.clear();
                continue;
            }
            if !(ch == '>' && cur.ends_with('=')) {
                d += match ch {
                    '(' | '[' | '{' | '<' => 1,
                    ')' | ']' | '}' | '>' => -1,
                    _ => 0,
                };
            }
            cur.push(ch);
        }
        sigs.push((name, params));
        at = at + e.max(1);
    }
    sigs
}

fn js_breaks(old_src: &str, new_src: &str) -> (Vec<(String, String)>, HashSet<String>) {
    let (old, new) = (js_signatures(old_src), js_signatures(new_src));
    let mut out = vec![];
    for (name, a) in &old {
        let Some((_, b)) = new.iter().find(|(n, _)| n == name) else {
            if !Re::new(&format!(r"export\s[^;\n]*\b{}\b", regex::escape(name)), false).map_or(false, |r| r.is_match(new_src)) {
                out.push((format!("{name}() is no longer exported"), name.clone())); // still exported some other way: fine
            }
            continue;
        };
        if b.len() < a.len() && !b.last().map_or(false, |l| l.0.starts_with("...")) {
            out.push((format!("{name}() takes fewer parameters ({} -> {})", a.len(), b.len()), name.clone()));
        }
        if b.iter().filter(|(_, o)| !o).count() > a.iter().filter(|(_, o)| !o).count() {
            out.push((format!("{name}() requires more arguments than before"), name.clone()));
        }
    }
    (out, new.into_iter().map(|(n, _)| n).collect())
}

/// Public signatures this change breaks, as "file: what". Skips test files, new and deleted files, a function that moved
/// to another changed file, and anything whose name the request mentions (the user asked for it).
pub async fn signature_breaks(cwd: &Path, changed: &[String], request: &str) -> Vec<String> {
    let mut found: Vec<(String, String, String)> = vec![];
    let mut defined: HashSet<String> = HashSet::new();
    let asked: HashSet<String> = lit(r"\w+").find_all(&request.to_lowercase());
    for f in changed {
        let suffix = Path::new(f).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        if (suffix != "py" && !JS_SUFFIX.contains(&suffix.as_str())) || test_file().is_match(f) || !cwd.join(f).is_file() {
            continue;
        }
        let (code, old) = git(cwd, &["show", &format!("HEAD:{f}")]).await;
        if code != 0 {
            continue;
        }
        let Ok(new) = std::fs::read(cwd.join(f)).map(|b| String::from_utf8_lossy(&b).to_string()) else { continue };
        let old = String::from_utf8_lossy(&old).to_string();
        let (breaks, names) = if suffix == "py" { py_breaks(&old, &new) } else { js_breaks(&old, &new) };
        defined.extend(names);
        found.extend(breaks.into_iter().map(|(w, a)| (f.clone(), w, a)));
    }
    found
        .into_iter()
        .filter(|(_, what, about)| !asked.contains(&about.to_lowercase()) && !((what.contains("removed") || what.contains("exported")) && defined.contains(about)))
        .map(|(f, what, _)| format!("{f}: {what}"))
        .collect()
}

trait FindAll {
    fn find_all(&self, text: &str) -> HashSet<String>;
}
impl FindAll for Re {
    fn find_all(&self, text: &str) -> HashSet<String> {
        let (mut out, mut at) = (HashSet::new(), 0);
        while at < text.len() {
            let Some((s, e)) = self.find(&text[at..]) else { break };
            out.insert(text[at + s..at + e].to_string());
            at += e.max(1);
        }
        out
    }
}

pub fn signature_prompt(breaks: &[String]) -> String {
    format!(
        "Your change alters public signatures that code outside this diff may rely on (other modules, other repos, callers no test here covers):\n{}\n\nUnless the user asked for that, keep old calls working: keep the parameter and give it a safe default (e.g. `seen=None`, then create the value inside), or keep the old name as a thin wrapper. Then re-run the check. If the break is really needed, leave it and say so in one sentence of your final answer.",
        breaks.iter().take(10).map(|b| format!("- {b}")).collect::<Vec<_>>().join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_parameter_is_caught() {
        let old = "def unique_by(items, key, seen=set()):\n    return 1\n";
        let new = "def unique_by(items, key):\n    return 1\n";
        let (b, _) = py_breaks(old, new);
        assert_eq!(b[0].0, "unique_by() no longer accepts `seen`");
        let added = "def unique_by(items, key, extra, seen=set()):\n    return 1\n";
        assert!(py_breaks(old, added).0.iter().any(|(w, _)| w.contains("now requires `extra`")));
        assert!(py_breaks(old, "def other(): pass\n").0[0].0.contains("removed"));
    }

    #[test]
    fn js_exports() {
        let (b, _) = js_breaks("export function f(a: string, b = 1) {}\n", "export function f(a: string) {}\n");
        assert!(b[0].0.contains("fewer parameters"));
        let (b, _) = js_breaks("export const g = async (a) => 1;\n", "const g = 2;\n");
        assert!(b[0].0.contains("no longer exported"));
    }

    #[test]
    fn generated_dirs_and_static_only_edits_stay_out_of_checks() {
        let mut after = Snapshot::new();
        after.insert("src/a.ts".into(), "1".into());
        after.insert("apps/web/.next.stale-typecheck/BUILD_ID".into(), "2".into());
        assert_eq!(changed_since(Some(&Snapshot::new()), Some(&after)), vec!["src/a.ts"]);
        let rule = crate::config::VerifyRule { paths: vec!["apps/web".into()], cwd: "apps/web".into(), commands: vec!["pnpm typecheck".into()], tests_glob: None };
        let mut cfg = Config::default();
        cfg.verify_rules = vec![rule];
        assert!(select_checks(&cfg, &["apps/web/public/demo.html".to_string()]).is_empty());
        assert_eq!(select_checks(&cfg, &["apps/web/src/a.ts".to_string()]).len(), 1);
    }

    #[test]
    fn errors_that_only_moved_lines_are_the_same_errors() {
        let a = error_lines("src/a.ts(10,5): error TS2307: Cannot find module 'x'\nok");
        let b = error_lines("src/a.ts(42,5): error TS2307: Cannot find module 'x'");
        assert_eq!(a, b);
        assert!(!error_lines("src/a.ts(1,1): error TS1: other").is_subset(&b));
    }

    #[tokio::test]
    async fn a_failure_that_the_clean_tree_has_too_is_not_the_runs() {
        let d = tempfile::tempdir().unwrap();
        let sh = |a: &str| { std::process::Command::new("sh").arg("-c").arg(a).current_dir(d.path()).output().unwrap(); };
        sh("git init -q && git config user.email t@t && git config user.name t && echo good > a.txt && git add -A && git commit -qm i");
        let r = backup(d.path()).await.unwrap();
        std::fs::write(d.path().join("a.txt"), "edited\n").unwrap(); // the run's change
        let cfg = Config::default();
        let run = |cmd: &str| { let (cfg, dir) = (cfg.clone(), d.path().to_path_buf()); let cmd = cmd.to_string(); async move { (run_check(&cmd, &dir, &cfg).await, dir) } };
        let changed = vec!["a.txt".to_string()];

        let stale = run("echo 'x.ts(3,1): error TS2307: stale' ; exit 1").await;
        let mine = run("grep -q good a.txt || { echo 'error: lost the good line'; exit 1; }").await;
        let mut cache = BTreeMap::new();
        let (new, old) = split_preexisting(d.path(), &r, &changed, &[], vec![stale, mine], &mut cache, &cfg).await;
        assert_eq!(old.len(), 1);
        assert!(old[0].command.contains("stale"));
        assert_eq!(new.len(), 1);
        assert!(new[0].command.contains("lost the good"));
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "edited\n"); // the agent's file is back
    }

    #[test]
    fn rel() {
        assert_eq!(rel_from("apps/web/src/a.ts", "apps/web"), "src/a.ts");
        assert_eq!(rel_from("x.ts", "apps"), "../x.ts");
    }

    #[test]
    fn snapshots() {
        let mut a = Snapshot::new();
        a.insert("a".into(), "1".into());
        let mut b = a.clone();
        b.insert("b".into(), "2".into());
        assert_eq!(changed_since(Some(&a), Some(&b)), vec!["b"]);
        assert!(changed_since(None, Some(&b)).is_empty());
    }
}
