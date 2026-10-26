//! A small, token-budgeted map of the repo for the system prompt: the files other files depend on most, each with its
//! top-level symbols. Files are ranked with PageRank over the import graph, so the core modules come first and leaf
//! scripts are dropped when the budget runs out.
use crate::util::importgraph;
use crate::util::outline::outline;
use std::collections::HashMap;
use std::path::Path;

const SOURCE: &[&str] = &["py", "ts", "tsx", "js", "jsx", "go", "rs"];
const MAX_FILES: usize = 400;

fn files(cwd: &Path) -> Vec<String> {
    let Ok(o) = std::process::Command::new("git").arg("ls-files").current_dir(cwd).output() else { return vec![] };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|f| {
            let p = Path::new(f);
            p.extension().map_or(false, |e| SOURCE.contains(&e.to_string_lossy().as_ref())) && !f.to_lowercase().contains("test") && p.file_name().map_or(true, |n| n != "__init__.py")
        })
        .take(MAX_FILES)
        .map(String::from)
        .collect()
}

pub fn pagerank(links: &HashMap<String, Vec<String>>, rounds: usize, d: f64) -> HashMap<String, f64> {
    let nodes: Vec<&String> = links.keys().collect();
    let n = nodes.len().max(1) as f64;
    let mut rank: HashMap<&String, f64> = nodes.iter().map(|k| (*k, 1.0 / n)).collect();
    for _ in 0..rounds {
        let mut nxt: HashMap<&String, f64> = nodes.iter().map(|k| (*k, (1.0 - d) / n)).collect();
        for (k, outs) in links {
            let share = rank[k] / (if outs.is_empty() { n } else { outs.len() as f64 });
            if outs.is_empty() {
                for t in &nodes {
                    *nxt.get_mut(*t).unwrap() += d * share;
                }
            } else {
                for t in outs {
                    if let Some(v) = nxt.get_mut(t) {
                        *v += d * share;
                    }
                }
            }
        }
        rank = nxt;
    }
    rank.into_iter().map(|(k, v)| (k.clone(), v)).collect()
}

pub fn build(cwd: &Path) -> Option<String> {
    build_with(cwd, 4000)
}

pub fn build_with(cwd: &Path, budget_chars: usize) -> Option<String> {
    let files = files(cwd);
    let srcs: HashMap<String, String> = files.iter().filter_map(|f| std::fs::read_to_string(cwd.join(f)).ok().map(|s| (f.clone(), s))).collect();
    if srcs.is_empty() {
        return None;
    }
    let live = importgraph::load(cwd);
    let links: HashMap<String, Vec<String>> = srcs
        .keys()
        .map(|f| (f.clone(), live.out.get(f).map(|s| s.iter().filter(|t| srcs.contains_key(*t)).cloned().collect()).unwrap_or_default()))
        .collect();
    let rank = pagerank(&links, 20, 0.85);
    let mut order: Vec<&String> = srcs.keys().collect();
    order.sort_by(|a, b| rank[*b].partial_cmp(&rank[*a]).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(b)));
    let (mut lines, mut used) = (vec![], 0usize);
    for f in order {
        let name = Path::new(f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let names: Vec<String> = outline(&name, &srcs[f]).into_iter().filter(|s| s.indent == 0 && !s.name.starts_with('_')).map(|s| s.name).take(8).collect();
        let line = if names.is_empty() { f.clone() } else { format!("{f}: {}", names.join(", ")) };
        if used + line.len() > budget_chars {
            break;
        }
        used += line.len() + 1;
        lines.push(line);
    }
    Some(lines.join("\n"))
}
