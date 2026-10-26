use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use crate::util::outline::outline;
use crate::util::paths::{is_binary_file, resolve_path};
use crate::util::search::similar_paths;
use crate::util::text::estimate_tokens;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

const WHOLE_FILE_LINES: usize = 400;
const MIN_SLICE_LINES: usize = 200;
const PI_MAX_LINES: usize = 2000;
const PI_MAX_BYTES: usize = 50 * 1024;
const MAX_FILE_SIZE: u64 = 1024 * 1024 * 10;
const MAX_OUTPUT_TOKENS: usize = 8000; // ~32KB; bigger files are read in slices with offset/limit

/// Pi's read: plain text from `offset`, cut at 2000 lines or 50KB (whichever comes first, never mid-line), with the
/// exact offset to continue from. Example: a 5000-line file gives lines 1-2000, then
/// '[Showing lines 1-2000 of 5000. Use offset=2001 to continue.]'.
pub fn pi_read(content: &str, offset: usize, limit: Option<usize>) -> String {
    let all: Vec<&str> = content.split('\n').collect();
    let total = all.len();
    let start = offset.saturating_sub(1);
    if start >= total {
        return format!("Offset {offset} is beyond end of file ({total} lines total)");
    }
    let end = limit.map_or(total, |l| (start + l).min(total));
    let (mut picked, mut size) = (Vec::new(), 0usize);
    for line in all[start..end].iter().take(PI_MAX_LINES) {
        size += line.len() + usize::from(!picked.is_empty());
        if size > PI_MAX_BYTES {
            break;
        }
        picked.push(*line);
    }
    if picked.is_empty() {
        return format!("[Line {} is over {}KB. Use shell: sed -n '{}p' <file> | head -c {PI_MAX_BYTES}]", start + 1, PI_MAX_BYTES / 1024, start + 1);
    }
    let shown_to = start + picked.len();
    let text = picked.join("\n");
    if shown_to < end {
        return format!("{text}\n\n[Showing lines {}-{shown_to} of {total}. Use offset={} to continue.]", start + 1, shown_to + 1);
    }
    if shown_to < total {
        return format!("{text}\n\n[{} more lines in file. Use offset={} to continue.]", total - shown_to, shown_to + 1);
    }
    text
}

/// Sub-ranges of [start, end] (1-based, inclusive) not covered by `have`.
fn missing(have: &[(usize, usize)], start: usize, end: usize) -> Vec<(usize, usize)> {
    let (mut out, mut cur) = (vec![], start);
    let mut sorted = have.to_vec();
    sorted.sort();
    for (a, b) in sorted {
        if b < cur {
            continue;
        }
        if a > end {
            break;
        }
        if a > cur {
            out.push((cur, a - 1));
        }
        cur = cur.max(b + 1);
    }
    if cur <= end {
        out.push((cur, end));
    }
    out
}

fn add(have: &mut Vec<(usize, usize)>, a: usize, b: usize) {
    have.push((a, b));
    have.sort();
    let mut merged = vec![have[0]];
    for &(x, y) in &have[1..] {
        let last = merged.last_mut().unwrap();
        if x <= last.1 + 1 {
            last.1 = last.1.max(y);
        } else {
            merged.push((x, y));
        }
    }
    *have = merged;
}

type Stamp = (u128, u64, u64);

#[derive(Default)]
struct State {
    seen: HashMap<String, (Stamp, Vec<(usize, usize)>)>,
    /// Whole-file text the model last got, kept here (not in the prompt) so a changed file can be answered with a diff.
    snap: HashMap<String, (u64, Vec<String>)>,
}

pub struct ReadFileTool {
    config: ConfigHandle,
    state: Mutex<State>,
}

impl ReadFileTool {
    pub fn new(config: ConfigHandle) -> Self {
        ReadFileTool { config, state: Mutex::new(State::default()) }
    }

    /// The model no longer has this file's text (its read result was cleared): the next read sends it again.
    pub fn forget(&self, path: &Path) {
        let k = path.to_string_lossy().to_string();
        let mut s = self.state.lock().unwrap();
        s.seen.remove(&k);
        s.snap.remove(&k);
    }

    fn unified(old: &[String], new: &[String]) -> String {
        let (a, b) = (old.join("\n"), new.join("\n"));
        let d = similar::TextDiff::from_lines(&a, &b);
        d.unified_diff().context_radius(2).header("before", "now").to_string().trim_end().to_string()
    }

    /// Whole-file reads only: a file the model already holds is not sent again. Unchanged gives one line, changed gives
    /// a diff against what it last saw. Slices and files over pi's cap go out as plain pi reads.
    fn pi_incremental(&self, path: &Path, content: &str, offset: usize, limit: Option<usize>, epoch: u64) -> String {
        let text = pi_read(content, offset, limit);
        if offset != 1 || limit.is_some() {
            return text;
        }
        let key = path.to_string_lossy().to_string();
        let lines: Vec<String> = content.split('\n').map(String::from).collect();
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut st = self.state.lock().unwrap();
        if let Some((e, old)) = st.snap.get(&key).cloned() {
            if e == epoch {
                if old == lines {
                    return format!("[{name} is unchanged since you last read it ({} lines) and is still in context above]", lines.len());
                }
                let diff = Self::unified(&old, &lines);
                if (diff.len() as f64) < 0.6 * content.len() as f64 {
                    st.snap.insert(key, (epoch, lines.clone()));
                    return format!("[{name} changed since you last read it ({} lines now); only the changes are shown, the rest is as you saw it]\n{diff}", lines.len());
                }
            }
        }
        if !text.contains("Use offset=") {
            // a cut-off read leaves the model without the whole file, so nothing to dedup against
            st.snap.insert(key, (epoch, lines));
        }
        text
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> String {
        if self.config.get().pi_mode {
            return format!("Read the contents of a text file. Output is truncated to {PI_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.", PI_MAX_BYTES / 1024);
        }
        "Read the contents of a text file. Returns the file content with line numbers. Files up to 400 lines come whole, so just call it with the path. For longer files pass `symbol` (see `outline`) or a wide offset/limit; many small slices cost more than one read. Cannot read binary files (images, executables, etc.). Read a file before you edit it. Do not re-read a file you just edited: the edit would have failed if it had not applied.".into()
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
                "path": {"type": "string", "description": "Path to the file to read (relative to working directory or absolute)"},
                "offset": {"type": "integer", "minimum": 1, "default": 1, "description": "Line number to start reading from (1-based). Defaults to 1"},
                "limit": {"type": ["integer", "null"], "minimum": 1, "description": "Maximum number of lines to read. If not specified, reads entire file."},
                "symbol": {"type": ["string", "null"], "description": "Read just this function/class/method (see `outline`) instead of a line range"},
            }),
            &["path"],
        )
    }

    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let cfg = self.config.get();
        let p = inv.str("path").unwrap_or("");
        let mut offset = inv.params["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let mut limit = inv.params["limit"].as_u64().map(|l| l as usize);
        let symbol = inv.str("symbol").filter(|s| !s.is_empty());
        let mut path = resolve_path(&inv.cwd, p);
        // models sometimes trail junk after the real name ("a.html એકસinvalid?"): if the first word is a file, it was meant
        if !path.exists() {
            let first = p.split_whitespace().next().unwrap_or("");
            for guess in [first, p.trim().trim_end_matches(|c| "?!.,".contains(c))] {
                let cut = resolve_path(&inv.cwd, guess);
                if guess != p && cut.is_file() {
                    path = cut;
                    break;
                }
            }
        }
        if !path.exists() {
            return ToolResult::error(format!("File not found: {}.{}", path.display(), similar_paths(&inv.cwd, &path, &cfg.forbidden_paths)));
        }
        if let Some(sym) = symbol {
            let src = std::fs::read(&path).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let syms: Vec<_> = outline(&name, &src).into_iter().filter(|x| x.name == sym).collect();
            if syms.is_empty() {
                return ToolResult::error(format!("No symbol '{sym}' in {name}. Call outline to list them."));
            }
            if syms.len() > 1 {
                let where_ = syms.iter().map(|x| format!("{}-{}", x.start, x.end)).collect::<Vec<_>>().join(", ");
                return ToolResult::error(format!("'{sym}' appears {} times ({where_}); read one with offset/limit.", syms.len()));
            }
            offset = syms[0].start;
            limit = Some((syms[0].end - syms[0].start + 1).min(400));
        }
        if !path.is_file() {
            return ToolResult::error(format!("Path is not a file: {}", path.display()));
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size > MAX_FILE_SIZE {
            return ToolResult::error(format!("File too large ({:.1}MB). Maximum is {}MB.", size as f64 / 1048576.0, MAX_FILE_SIZE / 1048576));
        }
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if is_binary_file(&path) {
            let mb = size as f64 / 1048576.0;
            let s = if mb >= 1.0 { format!("{mb:.2}MB") } else { format!("{size} bytes") };
            return ToolResult::error(format!("Cannot read binary file: {name} ({s}) This tool only reads text files."));
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return ToolResult::error(format!("Failed to read file: {e}")),
        };
        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => e.as_bytes().iter().map(|&b| b as char).collect(), // latin-1, as the Python harness fell back to
        };
        if cfg.pi_mode {
            return ToolResult::ok(self.pi_incremental(&path, &content, offset, limit, inv.epoch));
        }
        let lines: Vec<&str> = crate::util::search::split_lines(&content);
        let total = lines.len();
        if total == 0 {
            return ToolResult::ok_with("File is empty.", json!({"lines": 0}));
        }
        let mut start_idx = offset.saturating_sub(1);
        let mut end_idx = limit.map_or(total, |l| (start_idx + l).min(total));
        // Small slices cost an extra model round trip each (the whole history is sent again), far more than a few more
        // lines: a 400-line file is read whole, and any other slice grows to a useful size.
        if symbol.is_none() {
            if total <= WHOLE_FILE_LINES {
                start_idx = 0;
                end_idx = total;
            } else {
                end_idx = total.min(end_idx.max(start_idx + MIN_SLICE_LINES));
            }
        }
        let key = path.to_string_lossy().to_string();
        let md = std::fs::metadata(&path).ok();
        let stamp: Stamp = (md.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos()), size, inv.epoch);
        let mut st = self.state.lock().unwrap();
        let lines_owned: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        let old_lines = st.snap.get(&key).filter(|s| s.0 == inv.epoch).map(|s| s.1.clone());
        if st.seen.get(&key).map_or(true, |e| e.0 != stamp) {
            st.seen.insert(key.clone(), (stamp, vec![]));
        }
        let have_empty = st.seen[&key].1.is_empty();
        if let Some(old) = &old_lines {
            if have_empty && offset == 1 && limit.is_none() && *old != lines_owned {
                let diff = Self::unified(old, &lines_owned);
                if (diff.len() as f64) < 0.6 * content.len() as f64 {
                    st.seen.get_mut(&key).unwrap().1.push((1, total));
                    st.snap.insert(key, (inv.epoch, lines_owned));
                    return ToolResult::ok_with(
                        format!("[{name} changed since you last read it ({total} lines now); only the changes are shown, the rest is as you saw it]\n{diff}"),
                        json!({"path": path.to_string_lossy(), "total_lines": total, "diff_only": true}),
                    );
                }
            }
        }
        let have = st.seen[&key].1.clone();
        let need = missing(&have, start_idx + 1, end_idx);
        if need.is_empty() {
            return ToolResult::ok_with(format!("[lines {}-{end_idx} of {name} are unchanged and already in context above]", start_idx + 1), json!({"deduped": true}));
        }
        let mut formatted: Vec<String> = vec![];
        for (n, (a, b)) in need.iter().enumerate() {
            if n > 0 || *a > start_idx + 1 {
                let from = if n == 0 { start_idx + 1 } else { need[n - 1].1 + 1 };
                formatted.push(format!("[lines {from}-{} already in context above]", a - 1));
            }
            for i in *a..=*b {
                formatted.push(format!("{i}|{}", lines[i - 1]));
            }
        }
        if need.last().unwrap().1 < end_idx {
            formatted.push(format!("[lines {}-{end_idx} already in context above]", need.last().unwrap().1 + 1));
        }
        let mut output = formatted.join("\n");
        let mut truncated = false;
        if estimate_tokens(&output) > MAX_OUTPUT_TOKENS {
            let cap = MAX_OUTPUT_TOKENS * 4;
            let mut cut = String::new();
            for l in output.split('\n') {
                if cut.len() + l.len() + 1 > cap {
                    break;
                }
                cut.push_str(l);
                cut.push('\n');
            }
            output = format!("{}\n... [truncated; file has {total} lines. Re-read a slice with offset/limit]", cut.trim_end());
            truncated = true;
        }
        // what actually reached the model (truncation may cut it)
        let last = output.lines().filter_map(|l| l.split_once('|').and_then(|(n, _)| n.parse::<usize>().ok())).last();
        if let Some(last) = last {
            let entry = st.seen.get_mut(&key).unwrap();
            for (a, b) in &need {
                if *a <= last {
                    add(&mut entry.1, *a, (*b).min(last));
                }
            }
        }
        if st.seen[&key].1 == vec![(1, total)] {
            st.snap.insert(key, (inv.epoch, lines_owned));
        }
        if start_idx > 0 || end_idx < total {
            output = format!("Showing lines {}-{end_idx} of {total}\n\n{output}", start_idx + 1);
        }
        let mut r = ToolResult::ok_with(output, json!({"path": path.to_string_lossy(), "total_lines": total, "shown_start": start_idx + 1, "shown_end": end_idx}));
        r.truncated = truncated;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pi_read_cuts_with_hint() {
        let text = (1..=3000).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        let out = pi_read(&text, 1, None);
        assert!(out.ends_with("[Showing lines 1-2000 of 3000. Use offset=2001 to continue.]"));
        assert_eq!(pi_read("a\nb", 1, Some(1)), "a\n\n[1 more lines in file. Use offset=2 to continue.]");
        assert!(pi_read("a", 5, None).starts_with("Offset 5"));
    }
    #[test]
    fn ranges() {
        assert_eq!(missing(&[(1, 5), (10, 12)], 3, 11), vec![(6, 9)]);
        let mut h = vec![(1, 3)];
        add(&mut h, 4, 6);
        assert_eq!(h, vec![(1, 6)]);
    }
}
