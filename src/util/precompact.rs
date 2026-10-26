//! Free cleanup of tool output before it goes to the summarizer, so no model tokens are spent on terminal noise.
//! Example: a 40-line pip progress bar, ANSI colours and the same lint error repeated 30 times shrink to a few lines.
use crate::util::re::{lit, Re};
use std::collections::HashMap;
use std::sync::OnceLock;

fn ansi() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07"))
}
fn progress() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"(\d{1,3}%\|?[#=█▏▎▍▌▋▊▉ -]*|[#=█]{8,}|\.{10,})"))
}
fn digits() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"\d+"))
}

/// A run of progress-bar lines (or one line redrawn with \r) keeps only its last state.
fn squash_progress(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for line in lines {
        let line = line.rsplit('\r').next().unwrap_or("").to_string();
        if progress().is_match(&line) && out.last().map_or(false, |l| progress().is_match(l)) {
            *out.last_mut().unwrap() = line;
        } else {
            out.push(line);
        }
    }
    out
}

/// The same line more than `keep` times (a lint rule firing everywhere) keeps the first few, then a count.
fn dedup(lines: Vec<String>, keep: usize) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let (mut out, mut dropped) = (vec![], 0usize);
    for line in lines {
        let key = digits().replace_all(line.trim(), "N").to_string();
        if key.chars().count() < 12 || line.starts_with(' ') || line.starts_with('\t') {
            out.push(line); // indented lines are stack frames, handled separately
            continue;
        }
        let n = seen.entry(key).or_insert(0);
        *n += 1;
        if *n <= keep {
            out.push(line);
        } else {
            dropped += 1;
        }
    }
    if dropped > 0 {
        out.push(format!("[{dropped} more near-identical lines dropped]"));
    }
    out
}

/// A stack of frames longer than top+bottom keeps its first and last frames.
fn trim_traces(lines: Vec<String>, top: usize, bottom: usize) -> Vec<String> {
    let is_frame = |l: &str| l.starts_with("  File ") || l.starts_with("    ") || l.starts_with("\tat ") || lit(r"^\s+at ").is_match(l);
    let mut out = vec![];
    let mut i = 0;
    while i < lines.len() {
        let mut j = i;
        while j < lines.len() && is_frame(&lines[j]) {
            j += 1;
        }
        if j - i > (top + bottom) * 2 {
            let n = (top + bottom) * 2;
            out.extend_from_slice(&lines[i..i + top * 2]);
            out.push(format!("  [{} stack lines omitted]", j - i - n));
            out.extend_from_slice(&lines[j - bottom * 2..j]);
            i = j;
        } else {
            out.push(lines[i].clone());
            i += 1;
        }
    }
    out
}

pub fn pre_compact(text: &str) -> String {
    if text.chars().count() < 400 {
        return text.to_string();
    }
    let stripped = ansi().replace_all(text, "").to_string();
    let lines: Vec<String> = stripped.split('\n').map(String::from).collect();
    dedup(trim_traces(squash_progress(lines), 3, 3), 3).join("\n")
}
