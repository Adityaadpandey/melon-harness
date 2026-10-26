//! Folder instructions that show up when the agent first touches that folder, like Claude Code's nested CLAUDE.md.
//! Reading apps/web/src/a.tsx surfaces, once per session, any AGENTS.md / CLAUDE.md in apps/web/src, apps/web, apps and
//! the repo root, plus a folder's Command Code taste. A file may pull in others with `@path` lines (3 levels deep).
use crate::config::loader::{taste_files, taste_text};
use crate::util::paths::{expanduser, resolve};
use crate::util::re::{lit, Re};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];
const MAX_DEPTH: usize = 3;
const FILE_CHARS: usize = 3000;
const TOTAL_CHARS: usize = 6000;

fn import_re() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"(?m)^\s*@([\w./~-]+\.(?:md|txt))\s*$"))
}

fn inline(text: &str, base: &Path, root: &Path, depth: usize, done: &mut HashSet<PathBuf>) -> String {
    let re = import_re();
    let mut out = String::new();
    let mut at = 0;
    let home = resolve(&dirs::home_dir().unwrap_or_default());
    while let Some(c) = re.captures(&text[at..]) {
        let (s, e) = re.find(&text[at..]).unwrap();
        out.push_str(&text[at..at + s]);
        let whole = &text[at + s..at + e];
        let rel = c[1].clone().unwrap_or_default();
        let t = expanduser(&rel);
        let target = resolve(&if t.is_absolute() { t } else { base.join(t) });
        let inside = target.starts_with(resolve(root)) || target.starts_with(&home);
        let replacement = if depth >= MAX_DEPTH || done.contains(&target) || !inside || !target.is_file() {
            whole.to_string()
        } else {
            done.insert(target.clone());
            match std::fs::read_to_string(&target) {
                Ok(body) => inline(&body, target.parent().unwrap_or(base), root, depth + 1, done).trim().to_string(),
                Err(_) => whole.to_string(),
            }
        };
        out.push_str(&replacement);
        at += e.max(1);
    }
    out.push_str(&text[at..]);
    out
}

/// Instruction text from folders between `path` and the repo root that have not been shown yet (outermost first).
pub fn for_path(root: &Path, path: &Path, seen: &mut HashSet<PathBuf>) -> String {
    let root = resolve(root);
    let start = resolve(path).parent().map(Path::to_path_buf).unwrap_or_default();
    if start != root && !start.starts_with(&root) {
        return String::new();
    }
    let mut folders: Vec<PathBuf> = start.ancestors().filter(|d| *d == root || d.starts_with(&root)).map(Path::to_path_buf).collect();
    folders.reverse();
    let (mut out, mut used): (Vec<String>, usize) = (vec![], 0);
    for d in folders {
        if !seen.insert(d.clone()) {
            continue;
        }
        for name in NAMES {
            let f = d.join(name);
            if !f.is_file() {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&f) else { continue };
            let mut done = HashSet::from([resolve(&f)]);
            let text = inline(&raw, &d, &root, 0, &mut done).trim().to_string();
            if !text.is_empty() && used < TOTAL_CHARS {
                let rel = f.strip_prefix(&root).unwrap_or(&f).to_string_lossy().to_string();
                out.push(format!("## {rel}\n{}", text.chars().take(FILE_CHARS).collect::<String>()));
                used += text.chars().count().min(FILE_CHARS);
            }
            break; // AGENTS.md wins over CLAUDE.md in the same folder
        }
        // outside the AGENTS.md budget: taste files are a few lines each
        if d != root {
            for f in taste_files(&d) {
                let t = taste_text(&f);
                if !t.is_empty() {
                    out.push(format!("## {} (learned taste, a default not a rule)\n{}", f.strip_prefix(&root).unwrap_or(&f).to_string_lossy(), t.chars().take(FILE_CHARS).collect::<String>()));
                }
            }
        }
    }
    if out.is_empty() {
        return String::new();
    }
    format!("[Instructions for this folder, shown once. Follow them for work here; they rank below the user's request.]\n\n{}", out.join("\n\n"))
}
