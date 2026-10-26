//! Usage and cost summary from the trace. Cost is only computed for models the user priced in config.
use crate::config::ModelPrice;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const NOISE_FLOOR: i64 = 1024; // smaller shortfalls are cache-breakpoint granularity, not misses
const CACHE_TTL_SEC: f64 = 300.0;

fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

pub fn cache_waste(events: &[Value], pricing: &HashMap<String, ModelPrice>) -> Value {
    let mut prev: Option<(i64, String, f64)> = None;
    let mut reported: HashSet<String> = HashSet::new();
    let (mut tokens, mut cost, mut misses) = (0i64, 0.0f64, 0usize);
    let mut reasons: Vec<String> = vec![];
    for e in events {
        match e["event"].as_str() {
            Some("compaction") => {
                prev = None;
                continue;
            }
            Some("model_call") if e["ok"] == true && e.get("tokens").map_or(false, |t| !t.is_null()) => {}
            _ => continue,
        }
        let t = &e["tokens"];
        let (prompt, cached) = (i(t, "prompt_tokens"), i(t, "cached_tokens"));
        let model = e["model"].as_str().unwrap_or("").to_string();
        if cached > 0 {
            reported.insert(model.clone());
        }
        let at = e["t"].as_f64().unwrap_or(0.0);
        if let Some((pp, pm, pend)) = &prev {
            if reported.contains(&model) {
                let shortfall = pp - cached;
                if shortfall > NOISE_FLOOR {
                    let idle = (at - e["sec"].as_f64().unwrap_or(0.0)) - pend;
                    let why = if *pm != model { "model changed" } else if idle > CACHE_TTL_SEC { "idle > 5 min" } else { "prefix changed" };
                    misses += 1;
                    tokens += shortfall;
                    if !reasons.iter().any(|r| r == why) {
                        reasons.push(why.to_string());
                    }
                    if let Some(p) = pricing.get(&model) {
                        if let Some(c) = p.cached_input {
                            cost += shortfall as f64 * (p.input - c) / 1_000_000.0;
                        }
                    }
                }
            }
        }
        prev = Some((prompt, model, at));
    }
    reasons.sort();
    json!({"missed_tokens": tokens, "misses": misses, "reasons": reasons,
           "extra_cost_usd": if cost > 0.0 { json!((cost * 10000.0).round() / 10000.0) } else { Value::Null }})
}

pub fn summarize_usage(events: &[Value], pricing: &HashMap<String, ModelPrice>) -> Value {
    let (mut prompt, mut completion, mut cached, mut calls) = (0i64, 0i64, 0i64, 0i64);
    let mut cost: Option<f64> = Some(0.0);
    for e in events {
        if e["event"] != "model_call" || e["ok"] != true || e.get("tokens").map_or(true, Value::is_null) {
            continue;
        }
        let t = &e["tokens"];
        calls += 1;
        let (p, c, k) = (i(t, "prompt_tokens"), i(t, "completion_tokens"), i(t, "cached_tokens"));
        prompt += p;
        completion += c;
        cached += k;
        match (pricing.get(e["model"].as_str().unwrap_or("")), cost) {
            (Some(price), Some(total)) => {
                let rate = price.cached_input.unwrap_or(price.input);
                cost = Some(total + ((p - k) as f64 * price.input + k as f64 * rate + c as f64 * price.output) / 1_000_000.0);
            }
            _ => cost = None,
        }
    }
    json!({
        "calls": calls, "prompt_tokens": prompt, "completion_tokens": completion, "cached_tokens": cached,
        "cache_hit_pct": if prompt > 0 { ((1000.0 * cached as f64 / prompt as f64).round()) / 10.0 } else { 0.0 },
        "est_cost_usd": match cost { Some(c) if calls > 0 => json!((c * 10000.0).round() / 10000.0), _ => Value::Null },
        "cache_waste": cache_waste(events, pricing),
        "heaviest": heaviest_results(events),
    })
}

/// What made the prompts big. A tool result is re-sent with every later model call, so its real cost is its size
/// times the calls that follow it (a compaction rewrites history, so it ends the count).
pub fn heaviest_results(events: &[Value]) -> Value {
    let mut rows: Vec<(String, String, i64, i64)> = vec![];
    let mut repeats: BTreeMap<(String, String), i64> = BTreeMap::new();
    let mut later = 0i64;
    for e in events.iter().rev() {
        match e["event"].as_str() {
            Some("model_call") if e["ok"] == true => later += 1,
            Some("compaction") => later = 0,
            Some("tool_call") if i(e, "chars") > 0 => {
                let tokens = i(e, "chars") / 4;
                let tool = e["tool"].as_str().unwrap_or("").to_string();
                let target = e["target"].as_str().unwrap_or("").to_string();
                rows.push((tool.clone(), target.clone(), tokens, tokens * later));
                *repeats.entry((tool, target)).or_default() += 1;
            }
            _ => {}
        }
    }
    rows.sort_by(|a, b| b.3.cmp(&a.3));
    let carried: i64 = rows.iter().map(|r| r.3).sum();
    let top: Vec<Value> = rows.iter().take(5).map(|r| json!({"tool": r.0, "target": r.1, "tokens": r.2, "carried": r.3})).collect();
    let repeated: Vec<String> = repeats
        .iter()
        .filter(|((t, _), n)| **n > 1 && matches!(t.as_str(), "read_file" | "grep" | "search"))
        .map(|((t, g), n)| format!("{t} {g} x{n}"))
        .take(5)
        .collect();
    json!({"top": top, "tool_result_carried": carried, "repeated": repeated})
}
