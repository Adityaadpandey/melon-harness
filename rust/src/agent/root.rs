//! Finding the project when melon was started one folder above it. Outside a repo the harness cannot see what changed,
//! so it skips checks, review and undo.
use crate::util::re::Re;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn in_repo(cwd: &Path) -> bool {
    cwd.ancestors().any(|d| d.join(".git").exists())
}

pub fn child_repos(cwd: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(cwd) else { return vec![] };
    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|d| d.is_dir() && !d.file_name().map_or(true, |n| n.to_string_lossy().starts_with('.')) && d.join(".git").exists()).collect();
    v.sort();
    v
}

/// The repo one level below `cwd` that this request is about, or None when cwd is already inside a repo or nothing says
/// which one. The signals, in order: it is the only repo there, the request names one, files the agent already touched
/// live in one, or the previous request used one.
pub fn pick_root(cwd: &Path, message: &str, hint: Option<&Path>, targets: &[String]) -> Option<PathBuf> {
    if in_repo(cwd) {
        return None;
    }
    let repos = child_repos(cwd);
    if repos.len() == 1 {
        return Some(repos[0].clone());
    }
    let named: Vec<&PathBuf> = repos
        .iter()
        .filter(|r| {
            let name = regex::escape(&r.file_name().unwrap().to_string_lossy());
            Re::new(&format!(r"(?:^|[^\w-]){name}(?:$|[^\w-])"), true).map_or(false, |re| re.is_match(message)) || message.contains(&*r.to_string_lossy())
        })
        .collect();
    if named.len() == 1 {
        return Some(named[0].clone());
    }
    let mut seen: HashMap<&PathBuf, usize> = HashMap::new();
    for t in targets {
        for r in &repos {
            if t.starts_with(&format!("{}/", r.display())) {
                *seen.entry(r).or_default() += 1;
            }
        }
    }
    if let Some((r, _)) = seen.into_iter().max_by_key(|(_, n)| *n) {
        return Some(r.clone());
    }
    hint.filter(|h| repos.iter().any(|r| r == h)).map(Path::to_path_buf)
}
