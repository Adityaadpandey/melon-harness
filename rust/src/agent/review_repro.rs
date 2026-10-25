//! Try to make a confirmed defect actually happen before the reader is told about it. A verifier that only reads code
//! can be fooled by a plausible chain; a failing script cannot. The repro agent works in a throwaway copy of the change
//! (never the user's checkout), with the OS sandbox on.
use std::path::{Path, PathBuf};

pub const REPRO_PROMPT: &str = "A reviewer claims this defect exists in the code under your current directory (a throwaway copy of the change):

{claim}

The verifier's evidence: {evidence}

Prove it by running code, or show that it does not happen. Write the smallest test or script that pushes the exact input from the claim through
the real function (or the real component, route or query) and prints what comes out. Put it in a NEW file named .melon-repro.<ext>; never edit an
existing file. Run it with the project's own runner (look at package.json / pyproject.toml / the neighbouring tests for the command; node_modules
is already linked in). No network, no installs, no database. If the defect needs something you cannot run here, say CANNOT_RUN and why.

Reply with EXACTLY one first line:
REPRODUCED | <the command you ran> | <the one output line that shows the defect>
or
NOT_REPRODUCED | <the command you ran> | <the output line that shows the code does the right thing>
or
CANNOT_RUN | <why>";

pub const REPRO_TOOLS: &[&str] = &["read_file", "grep", "glob", "list_dir", "search", "outline", "git", "shell", "write_file"];
pub const MAX_REPROS: usize = 4;

async fn git(cwd: &Path, args: &[&str]) -> String {
    tokio::process::Command::new("git").args(args).current_dir(cwd).stderr(std::process::Stdio::null()).output().await.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

/// A detached worktree of `rev_tip` holding the changed files as they are now, with node_modules linked in from the
/// real checkout. None when git refuses.
pub async fn make_scratch(cwd: &Path, files: &[String], rev_tip: &str, from_worktree: bool) -> Option<PathBuf> {
    let tree = tempfile::Builder::new().prefix("melon-repro-").tempdir().ok()?.keep();
    git(cwd, &["worktree", "add", "--detach", &tree.to_string_lossy(), rev_tip]).await;
    if !tree.join(".git").exists() {
        let _ = std::fs::remove_dir_all(&tree);
        return None;
    }
    if from_worktree {
        for f in files {
            let (src, dst) = (cwd.join(f), tree.join(f));
            if src.is_file() {
                if let Some(p) = dst.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                let _ = std::fs::copy(&src, &dst);
            } else if dst.exists() {
                let _ = std::fs::remove_file(&dst);
            }
        }
    }
    link_dependencies(cwd, &tree);
    Some(tree)
}

/// Symlink every node_modules (root and one or two folders down: apps/web/node_modules) into the copy.
pub fn link_dependencies(source: &Path, tree: &Path) {
    let mut candidates = vec![source.join("node_modules")];
    for lvl1 in std::fs::read_dir(source).into_iter().flatten().flatten() {
        let p = lvl1.path();
        if !p.is_dir() {
            continue;
        }
        candidates.push(p.join("node_modules"));
        for lvl2 in std::fs::read_dir(&p).into_iter().flatten().flatten() {
            candidates.push(lvl2.path().join("node_modules"));
        }
    }
    for nm in candidates {
        let Ok(rel) = nm.strip_prefix(source) else { continue };
        if !nm.is_dir() || rel.components().rev().skip(1).any(|c| c.as_os_str() == "node_modules") || tree.join(rel).exists() {
            continue;
        }
        if let Some(p) = tree.join(rel).parent() {
            let _ = std::fs::create_dir_all(p);
        }
        if let Ok(real) = std::fs::canonicalize(&nm) {
            let _ = std::os::unix::fs::symlink(real, tree.join(rel));
        }
    }
}

pub async fn drop_scratch(cwd: &Path, tree: &Path) {
    git(cwd, &["worktree", "remove", "--force", &tree.to_string_lossy()]).await;
    let _ = std::fs::remove_dir_all(tree);
}

/// (REPRODUCED | NOT_REPRODUCED | CANNOT_RUN, the rest of the line). Anything else reads as CANNOT_RUN: a reply nobody
/// can parse proves nothing.
pub fn parse_repro(text: &str) -> (String, String) {
    for line in text.lines() {
        let l = line.trim().trim_start_matches(|c| " -*•".contains(c));
        let (head, rest) = l.split_once('|').unwrap_or((l, ""));
        let word = head.trim().to_uppercase();
        if matches!(word.as_str(), "REPRODUCED" | "NOT_REPRODUCED" | "CANNOT_RUN") {
            return (word, rest.trim().chars().take(300).collect());
        }
    }
    ("CANNOT_RUN".into(), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses() {
        assert_eq!(parse_repro("- REPRODUCED | node x.js | TypeError").0, "REPRODUCED");
        assert_eq!(parse_repro("NOT_REPRODUCED | cmd | fine").1, "cmd | fine");
        assert_eq!(parse_repro("blah").0, "CANNOT_RUN");
        assert_eq!(parse_repro("CANNOT_RUN | no deps"), ("CANNOT_RUN".into(), "no deps".into()));
    }
}
