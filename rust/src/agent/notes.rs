//! Task notes: what a long job has decided and done, kept on disk so it survives compaction, /resume and a new session.
//! Stored outside the repo (data dir), one file per workspace.
use crate::config::loader::get_data_dir;
use crate::util::paths::{atomic_write, resolve};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};

pub const MAX_CHARS: usize = 3000;
const PROMPT_CHARS: usize = 1500;
const STALE_DAYS: f64 = 14.0;

pub fn path_for(cwd: &Path) -> PathBuf {
    let h = hex::encode(Sha1::digest(resolve(cwd).to_string_lossy().as_bytes()));
    get_data_dir().join("notes").join(format!("{}.md", &h[..16]))
}

pub fn read(cwd: &Path) -> String {
    std::fs::read_to_string(path_for(cwd)).map(|s| s.trim().to_string()).unwrap_or_default()
}

pub fn write(cwd: &Path, text: &str) {
    let p = path_for(cwd);
    if text.trim().is_empty() {
        let _ = std::fs::remove_file(&p);
        return;
    }
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = atomic_write(&p, &format!("{}\n", text.trim().chars().take(MAX_CHARS).collect::<String>()));
}

pub fn age_days(cwd: &Path) -> f64 {
    std::fs::metadata(path_for(cwd)).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map_or(0.0, |d| d.as_secs_f64() / 86400.0)
}

/// Notes from earlier work, for the start of a session. Old ones are dropped: a stale plan misleads more than it helps.
pub fn prompt_section(cwd: &Path) -> Option<String> {
    let text = read(cwd);
    let days = age_days(cwd);
    if text.is_empty() || days > STALE_DAYS {
        return None;
    }
    let when = if days < 1.0 { "today".to_string() } else { format!("{} day(s) ago", days as i64) };
    Some(format!(
        "# Task notes (written {when}, still yours to update)\n\n{}\n\nThese come from earlier work in this repo. If the request is about something else, ignore them and clear them with the notes tool.",
        text.chars().take(PROMPT_CHARS).collect::<String>()
    ))
}
