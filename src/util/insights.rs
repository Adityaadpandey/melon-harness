//! `melon insights`: what the trace files say about how runs actually went, so the harness is tuned from evidence
//! rather than from memory of one bad session. It answers three questions per model: does it answer (failures,
//! retries, speed, cache), does it use the tools properly (errors, malformed calls), and how often did the loop have to
//! step in (empty replies, tool calls written as text, cut-off replies, loops).
//!
//! Example: `melon insights --days 14` after a week on a new local model shows "bad calls 9%" and "empty reply x12",
//! which says to add a compat flag or a prompt note for it, not to blame the task.
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Default)]
struct Model {
    calls: i64,
    failed: i64,
    retried: i64,
    sec: f64,
    prompt: i64,
    cached: i64,
    output: i64,
    tools: i64,
    tool_errors: i64,
    bad_calls: i64,
    nudges: BTreeMap<String, i64>,
}

#[derive(Default)]
struct ToolStat {
    calls: i64,
    errors: i64,
    sec: f64,
    why: BTreeMap<String, i64>,
}

/// A malformed or made-up call, as opposed to a tool that ran and failed (a test that fails is not the model's wiring).
fn is_bad_call(error: &str) -> bool {
    ["Tool arguments were not valid JSON", "Invalid parameters", "Unknown tool"].iter().any(|p| error.starts_with(p))
}

fn pct(part: i64, whole: i64) -> String {
    if whole == 0 { "-".into() } else { format!("{:.0}%", 100.0 * part as f64 / whole as f64) }
}

/// The report for sessions given as lists of trace events (one list per session, in file order).
pub fn summarize(sessions: &[Vec<Value>]) -> Value {
    let (mut models, mut tools, mut tasks): (BTreeMap<String, Model>, BTreeMap<String, ToolStat>, BTreeMap<String, i64>) = Default::default();
    for events in sessions {
        // a tool call belongs to the model that asked for it: the last one that answered in this session
        let mut current = String::new();
        for e in events {
            match e["event"].as_str().unwrap_or("") {
                "model_call" => {
                    let name = e["model"].as_str().unwrap_or("?").to_string();
                    let m = models.entry(name.clone()).or_default();
                    m.calls += 1;
                    m.sec += e["sec"].as_f64().unwrap_or(0.0);
                    if e["attempt"].as_i64().unwrap_or(0) > 0 {
                        m.retried += 1;
                    }
                    if e["ok"] == true {
                        m.prompt += e["tokens"]["prompt_tokens"].as_i64().unwrap_or(0);
                        m.cached += e["tokens"]["cached_tokens"].as_i64().unwrap_or(0);
                        m.output += e["tokens"]["completion_tokens"].as_i64().unwrap_or(0);
                        if e["role"] != "summarize" && e["role"] != "fast" {
                            current = name;
                        }
                    } else {
                        m.failed += 1;
                    }
                }
                "tool_call" => {
                    let (ok, error) = (e["ok"] == true, e["error"].as_str().unwrap_or(""));
                    let t = tools.entry(e["tool"].as_str().unwrap_or("?").to_string()).or_default();
                    t.calls += 1;
                    t.sec += e["sec"].as_f64().unwrap_or(0.0);
                    if !ok {
                        t.errors += 1;
                        // the first line, numbers flattened, so "line 12" and "line 40" count as one cause
                        let first: String = error.lines().next().unwrap_or("").chars().take(70).map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
                        *t.why.entry(first).or_default() += 1;
                    }
                    if !current.is_empty() {
                        let m = models.entry(current.clone()).or_default();
                        m.tools += 1;
                        m.tool_errors += !ok as i64;
                        m.bad_calls += (!ok && is_bad_call(error)) as i64;
                    }
                }
                "nudge" if !current.is_empty() => *models.entry(current.clone()).or_default().nudges.entry(e["kind"].as_str().unwrap_or("?").to_string()).or_default() += 1,
                "task_end" => *tasks.entry(e["status"].as_str().unwrap_or("?").to_string()).or_default() += 1,
                _ => {}
            }
        }
    }
    let models: Vec<Value> = models
        .iter()
        .map(|(name, m)| {
            json!({"model": name, "calls": m.calls, "failed": m.failed, "retried": m.retried, "avg_sec": if m.calls > 0 { (m.sec / m.calls as f64 * 10.0).round() / 10.0 } else { 0.0 },
                   "prompt_tokens": m.prompt, "cached_tokens": m.cached, "output_tokens": m.output, "tool_calls": m.tools, "tool_errors": m.tool_errors, "bad_calls": m.bad_calls, "nudges": m.nudges})
        })
        .collect();
    let tools: Vec<Value> = tools
        .iter()
        .map(|(name, t)| {
            let mut why: Vec<(&String, &i64)> = t.why.iter().collect();
            why.sort_by(|a, b| b.1.cmp(a.1));
            json!({"tool": name, "calls": t.calls, "errors": t.errors, "avg_sec": if t.calls > 0 { (t.sec / t.calls as f64 * 100.0).round() / 100.0 } else { 0.0 },
                   "top_errors": why.iter().take(3).map(|(w, n)| json!({"error": w, "count": n})).collect::<Vec<_>>()})
        })
        .collect();
    json!({"sessions": sessions.len(), "tasks": tasks, "models": models, "tools": tools})
}

/// Things worth acting on, in plain words. Small samples are skipped: three calls prove nothing.
pub fn advice(report: &Value) -> Vec<String> {
    let mut out = vec![];
    for m in report["models"].as_array().into_iter().flatten() {
        let n = |k: &str| m[k].as_i64().unwrap_or(0);
        let name = m["model"].as_str().unwrap_or("?");
        if n("calls") < 20 {
            continue;
        }
        if n("failed") * 10 > n("calls") {
            out.push(format!("{name}: {} of its calls failed. Check the key, the plan, or put another model first in [routing].", pct(n("failed"), n("calls"))));
        }
        if n("tool_calls") >= 20 && n("bad_calls") * 20 > n("tool_calls") {
            out.push(format!("{name}: {} of its tool calls were malformed or named a tool that does not exist. Try `extra_body = {{ parallel_tool_calls = false }}` in its provider's compat table, or a lower temperature.", pct(n("bad_calls"), n("tool_calls"))));
        }
        if n("prompt_tokens") > 500_000 && n("cached_tokens") * 5 < n("prompt_tokens") {
            out.push(format!("{name}: only {} of its prompt tokens came from cache, so long sessions are billed in full each turn.", pct(n("cached_tokens"), n("prompt_tokens"))));
        }
        let nudges: i64 = m["nudges"].as_object().map(|o| o.values().filter_map(Value::as_i64).sum()).unwrap_or(0);
        if nudges * 10 > n("calls") {
            out.push(format!("{name}: the loop had to step in {nudges} times in {} calls ({}). A note for this family in src/prompts.rs (FAMILY_NOTES) or .melon/APPEND_SYSTEM.md is the usual fix.", n("calls"), m["nudges"]));
        }
    }
    for t in report["tools"].as_array().into_iter().flatten() {
        let (calls, errors) = (t["calls"].as_i64().unwrap_or(0), t["errors"].as_i64().unwrap_or(0));
        if calls >= 20 && errors * 4 > calls && t["tool"] != "shell" {
            out.push(format!("{}: {} of calls fail, mostly \"{}\". Its description or error text is probably misleading the model.", t["tool"].as_str().unwrap_or("?"), pct(errors, calls), t["top_errors"][0]["error"].as_str().unwrap_or("")));
        }
    }
    out
}

pub fn render(report: &Value, days: u64) -> String {
    let mut out = vec![format!("melon insights: {} session(s) in the last {days} day(s)", report["sessions"])];
    if let Some(t) = report["tasks"].as_object().filter(|t| !t.is_empty()) {
        out.push(format!("\nRequests by outcome: {}", t.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")));
    }
    out.push(format!("\n{:<38} {:>6} {:>6} {:>6} {:>7} {:>7} {:>9} {:>9}  {}", "MODEL", "calls", "failed", "retry", "avg s", "cached", "tool err", "bad call", "loop stepped in"));
    for m in report["models"].as_array().into_iter().flatten() {
        let n = |k: &str| m[k].as_i64().unwrap_or(0);
        let nudges = m["nudges"].as_object().map(|o| o.iter().map(|(k, v)| format!("{k} x{v}")).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        out.push(format!(
            "{:<38} {:>6} {:>6} {:>6} {:>7} {:>7} {:>9} {:>9}  {}",
            crate::util::text::head_chars(m["model"].as_str().unwrap_or("?"), 38), n("calls"), pct(n("failed"), n("calls")), n("retried"), format!("{:.1}", m["avg_sec"].as_f64().unwrap_or(0.0)), pct(n("cached_tokens"), n("prompt_tokens")), pct(n("tool_errors"), n("tool_calls")), pct(n("bad_calls"), n("tool_calls")), nudges
        ));
    }
    out.push(format!("\n{:<28} {:>6} {:>6} {:>7}  {}", "TOOL", "calls", "errors", "avg s", "most common error"));
    for t in report["tools"].as_array().into_iter().flatten() {
        let (calls, errors) = (t["calls"].as_i64().unwrap_or(0), t["errors"].as_i64().unwrap_or(0));
        out.push(format!("{:<28} {:>6} {:>6} {:>7}  {}", crate::util::text::head_chars(t["tool"].as_str().unwrap_or("?"), 28), calls, pct(errors, calls), format!("{:.2}", t["avg_sec"].as_f64().unwrap_or(0.0)), t["top_errors"][0]["error"].as_str().unwrap_or("")));
    }
    let tips = advice(report);
    if !tips.is_empty() {
        out.push(format!("\nWorth a look:\n{}", tips.iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n")));
    }
    out.join("\n")
}

/// Trace files touched in the last `days` days. Older ones are skipped by their timestamp alone, without being opened.
pub fn load(dir: &Path, days: u64) -> Vec<Vec<Value>> {
    let since = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86400);
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    rd.flatten()
        .filter(|e| e.metadata().and_then(|m| m.modified()).map_or(false, |t| t >= since))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .map(|text| text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect::<Vec<Value>>())
        .filter(|events| !events.is_empty())
        .collect()
}

/// `melon insights [--days N] [--json]`
pub fn main(args: Vec<String>) -> i32 {
    let days = args.iter().position(|a| a == "--days").and_then(|i| args.get(i + 1)).and_then(|d| d.parse().ok()).unwrap_or(7);
    let report = summarize(&load(&crate::config::loader::get_data_dir().join("traces"), days));
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    } else {
        println!("{}", render(&report, days));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_trouble_is_counted_against_the_model_that_made_the_call() {
        let mut events = vec![json!({"event": "model_call", "model": "qwen", "ok": true, "role": "default", "sec": 2.0, "tokens": {"prompt_tokens": 1000, "cached_tokens": 100, "completion_tokens": 50}})];
        events.extend((0..30).map(|i| json!({"event": "tool_call", "tool": "edit", "ok": i % 3 != 0, "sec": 0.1, "error": if i % 3 == 0 { json!("Invalid parameters: path is required") } else { Value::Null }})));
        events.push(json!({"event": "nudge", "kind": "empty_reply"}));
        events.extend((0..25).map(|_| json!({"event": "model_call", "model": "qwen", "ok": true, "role": "default", "sec": 1.0})));
        let r = summarize(&[events]);
        let m = &r["models"][0];
        assert_eq!((m["tool_calls"].as_i64(), m["bad_calls"].as_i64(), m["nudges"]["empty_reply"].as_i64()), (Some(30), Some(10), Some(1)));
        assert_eq!(r["tools"][0]["top_errors"][0]["count"], 10);
        let tips = advice(&r).join("\n");
        assert!(tips.contains("qwen: 33% of its tool calls were malformed") && tips.contains("edit: 33% of calls fail"), "{tips}");
        assert!(render(&r, 7).contains("empty_reply x1"));
    }
}
