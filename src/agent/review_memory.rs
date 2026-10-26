//! What earlier reviews of this repo already ruled out, so the same wrong claim is not raised every time. A claim the
//! verifier refuted is kept per workspace, outside the repo, for two weeks.
use crate::config::loader::get_data_dir;
use crate::util::paths::{atomic_write, resolve};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};

const KEEP_DAYS: f64 = 14.0;
const MAX_ENTRIES: usize = 15;

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

pub fn path_for(cwd: &Path, base: Option<&Path>) -> PathBuf {
    let base = base.map(Path::to_path_buf).unwrap_or_else(get_data_dir);
    let h = hex::encode(Sha1::digest(resolve(cwd).to_string_lossy().as_bytes()));
    base.join("review_memory").join(format!("{}.json", &h[..16]))
}

pub fn load(cwd: &Path, base: Option<&Path>) -> Vec<Value> {
    let rows: Vec<Value> = std::fs::read_to_string(path_for(cwd, base)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let fresh = now() - KEEP_DAYS * 86400.0;
    let mut rows: Vec<Value> = rows.into_iter().filter(|r| r.is_object() && r["at"].as_f64().unwrap_or(0.0) > fresh).collect();
    let n = rows.len();
    rows.drain(..n.saturating_sub(MAX_ENTRIES));
    rows
}

/// rejected = (where, what, why). Same file and wording is stored once.
pub fn remember(cwd: &Path, rejected: &[(String, String, String)], base: Option<&Path>) {
    if rejected.is_empty() {
        return;
    }
    let mut rows = load(cwd, base);
    let key = |w: &str, t: &str| (w.split(':').next().unwrap_or("").to_string(), t.chars().take(60).collect::<String>());
    let mut have: Vec<(String, String)> = rows.iter().map(|r| key(r["where"].as_str().unwrap_or(""), r["what"].as_str().unwrap_or(""))).collect();
    for (w, t, why) in rejected {
        let k = key(w, t);
        if !have.contains(&k) {
            rows.push(json!({"where": w, "what": t.chars().take(160).collect::<String>(), "why": why.chars().take(160).collect::<String>(), "at": now()}));
            have.push(k);
        }
    }
    let n = rows.len();
    let tail: Vec<Value> = rows.split_off(n.saturating_sub(MAX_ENTRIES));
    let p = path_for(cwd, base);
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = atomic_write(&p, &Value::Array(tail).to_string()); // a memory that cannot be written costs one repeated claim, nothing more
}

pub fn render(cwd: &Path, base: Option<&Path>) -> String {
    let rows = load(cwd, base);
    if rows.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = rows
        .iter()
        .map(|r| {
            let why = r["why"].as_str().filter(|w| !w.is_empty()).unwrap_or("the code contradicts it");
            format!("- {}: {} (ruled out because: {why})", r["where"].as_str().unwrap_or(""), r["what"].as_str().unwrap_or(""))
        })
        .collect();
    format!("ALREADY RULED OUT in earlier reviews of this repo (do not report these again unless the code has changed since):\n{}\n\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remembers_once() {
        let (d, repo) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        remember(repo.path(), &[("a.ts:3".into(), "write is unguarded".into(), "caller catches".into())], Some(d.path()));
        remember(repo.path(), &[("a.ts:9".into(), "write is unguarded".into(), "again".into())], Some(d.path()));
        assert_eq!(load(repo.path(), Some(d.path())).len(), 1);
        assert!(render(repo.path(), Some(d.path())).contains("caller catches"));
    }
}
