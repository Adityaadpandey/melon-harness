//! What the developer told melon about their repo, kept in `.melon/graph.toml` so it can be committed and shared:
//! which folders to leave out of the graph, which ones to keep even though they look dead, and what each area is for.
//! Most repos are not tidy (generated code, old experiments, fixtures next to real code), and a graph full of that gives
//! wrong callers. So melon drops the obvious noise by itself and asks about the rest.
//!
//!     ignore  = ["examples/", "legacy/", "*.stories.tsx"]   # a folder (trailing /) or a glob
//!     keep    = ["src/plugins/"]                            # looks unused, is loaded by name: stop asking
//!     [areas]
//!     "src/billing" = "Stripe webhooks and invoice PDFs"
use crate::util::fnmatch;
use crate::util::importgraph;
use crate::util::symgraph;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::Path;

#[derive(Default, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct GraphCfg {
    pub ignore: Vec<String>,
    pub keep: Vec<String>,
    pub areas: BTreeMap<String, String>,
}

fn file(cwd: &Path) -> std::path::PathBuf {
    cwd.join(".melon/graph.toml")
}

pub fn load(cwd: &Path) -> GraphCfg {
    std::fs::read_to_string(file(cwd)).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
}

pub fn save(cwd: &Path, cfg: &GraphCfg) -> std::io::Result<()> {
    let f = file(cwd);
    std::fs::create_dir_all(f.parent().unwrap())?;
    std::fs::write(f, toml::to_string_pretty(cfg).unwrap_or_default())
}

/// Never worth asking: dependency folders, build output and machine-written files.
pub fn is_noise(rel: &str) -> bool {
    const DIRS: &[&str] = &["node_modules", "vendor", "dist", ".next", ".nuxt", "__generated__", "coverage", "target", "__pycache__", ".venv", "venv"];
    const ENDS: &[&str] = &[".min.js", ".bundle.js", ".d.ts", ".generated.ts", ".pb.go", "_pb2.py", "_pb2_grpc.py"];
    rel.split('/').any(|seg| DIRS.contains(&seg)) || ENDS.iter().any(|e| rel.ends_with(e))
}

impl GraphCfg {
    pub fn is_ignored(&self, rel: &str) -> bool {
        self.ignore.iter().any(|p| {
            if let Some(dir) = p.strip_suffix('/') {
                rel == dir || rel.starts_with(p.as_str()) || rel.contains(&format!("/{p}"))
            } else {
                fnmatch::fnmatch(rel, p) || fnmatch::fnmatch(rel.rsplit('/').next().unwrap_or(rel), p)
            }
        })
    }

    fn decided(&self, dir: &str) -> bool {
        let d = format!("{dir}/");
        self.ignore.contains(&d) || self.keep.contains(&d) || self.areas.contains_key(dir)
    }
}

/// Folders that are usually not the product itself.
const SIDE: &[&str] = &["fixtures", "fixture", "examples", "example", "samples", "sample", "mocks", "mock", "legacy", "old", "archive", "deprecated", "scripts", "migrations", "docs", "demo", "testdata", "stories", "playground", "tmp"];

#[derive(Debug, PartialEq)]
enum Q {
    /// A folder that usually is not core code. Default: leave it out.
    Side { dir: String, files: usize, syms: usize },
    /// Nothing outside the folder imports it. Dead, or loaded by name? Default: keep.
    Unused { dir: String, files: usize },
    /// What is this area for?
    Area { dir: String, files: usize, syms: usize },
}

fn questions(cwd: &Path, cfg: &GraphCfg, limit: usize) -> Vec<Q> {
    let sg = symgraph::load(cwd);
    let ig = importgraph::load(cwd);
    // every folder down to two levels, with the files inside it
    let mut dirs: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for f in sg.files.keys() {
        let parts: Vec<&str> = f.split('/').collect();
        for n in 1..=parts.len().saturating_sub(1).min(2) {
            dirs.entry(parts[..n].join("/")).or_default().push(f);
        }
    }
    let syms = |fs: &[&String]| fs.iter().map(|f| sg.files[*f].defs.len()).sum::<usize>();
    let (mut side, mut unused, mut areas) = (vec![], vec![], vec![]);
    for (dir, fs) in &dirs {
        if cfg.decided(dir) || cfg.ignore.iter().any(|p| dir.starts_with(p.trim_end_matches('/')) && p.ends_with('/')) || fs.len() < 3 {
            continue;
        }
        let leaf = dir.rsplit('/').next().unwrap_or(dir);
        if SIDE.contains(&leaf) {
            side.push(Q::Side { dir: dir.clone(), files: fs.len(), syms: syms(fs) });
            continue;
        }
        let inside: BTreeSet<&str> = fs.iter().map(|f| f.as_str()).collect();
        let tracked = fs.iter().any(|f| ig.out.contains_key(*f));
        let imported_from_outside = fs.iter().any(|f| ig.inc.get(*f).map_or(false, |s| s.iter().any(|i| !inside.contains(i.as_str()))));
        let is_test = fs.iter().all(|f| importgraph::test_hint().is_match(f));
        if tracked && fs.len() >= 5 && !imported_from_outside && !is_test && dir.contains('/') {
            unused.push(Q::Unused { dir: dir.clone(), files: fs.len() });
        } else if !dir.contains('/') || fs.len() >= 8 {
            areas.push(Q::Area { dir: dir.clone(), files: fs.len(), syms: syms(fs) });
        }
    }
    areas.sort_by_key(|q| match q {
        Q::Area { syms, .. } => std::cmp::Reverse(*syms),
        _ => std::cmp::Reverse(0),
    });
    side.into_iter().chain(unused).chain(areas).take(limit).collect()
}

/// Ask the developer about their repo and save the answers. Returns how many answers were recorded.
pub fn interview(cwd: &Path, input: &mut dyn BufRead, out: &mut dyn Write) -> usize {
    let mut cfg = load(cwd);
    let qs = questions(cwd, &cfg, 10);
    if qs.is_empty() {
        let _ = writeln!(out, "Nothing left to ask: the graph already reflects what's in .melon/graph.toml.");
        return 0;
    }
    let _ = writeln!(out, "A few questions so the code graph is clean. Enter takes the default in [brackets]; q stops.\n");
    let mut answered = 0;
    for q in &qs {
        let (prompt, dir) = match q {
            Q::Side { dir, files, syms } => (format!("`{dir}/` ({files} files, {syms} symbols) looks like side material. Leave it out of the graph? [Y/n] "), dir),
            Q::Unused { dir, files } => (format!("Nothing outside `{dir}/` imports it ({files} files). Dead code to leave out? (n = it's loaded by name, keep) [y/N] "), dir),
            Q::Area { dir, files, syms } => (format!("What is `{dir}/` for? ({files} files, {syms} symbols; one line, Enter to skip) "), dir),
        };
        let _ = write!(out, "{prompt}");
        let _ = out.flush();
        let mut line = String::new();
        if input.read_line(&mut line).unwrap_or(0) == 0 || line.trim() == "q" {
            break;
        }
        let a = line.trim();
        let yes = matches!(a.to_lowercase().as_str(), "y" | "yes");
        let no = matches!(a.to_lowercase().as_str(), "n" | "no");
        match q {
            Q::Side { .. } if !no => cfg.ignore.push(format!("{dir}/")),
            Q::Side { .. } => cfg.keep.push(format!("{dir}/")),
            Q::Unused { .. } if yes => cfg.ignore.push(format!("{dir}/")),
            Q::Unused { .. } => cfg.keep.push(format!("{dir}/")),
            Q::Area { .. } if !a.is_empty() => {
                cfg.areas.insert(dir.clone(), a.to_string());
            }
            Q::Area { .. } => {}
        }
        answered += 1;
        let _ = save(cwd, &cfg); // after every answer, so ^C loses nothing
    }
    let _ = save(cwd, &cfg); // even with no answers: the file marks the repo as asked, so it isn't asked on every start
    let _ = writeln!(out, "\nSaved to .melon/graph.toml ({} left out, {} area notes). Commit it to share with your team.", cfg.ignore.len(), cfg.areas.len());
    answered
}

/// First time melon opens a repo: the graph is already building in the background, so use the wait to ask.
pub fn first_run(cwd: &Path) {
    use crate::ui::style::{paint, MUTED};
    let started = std::time::Instant::now();
    println!("{}", paint("While it indexes, a few quick questions keep the graph clean (q skips; `melon graph` asks again any time).\n", MUTED));
    interview(cwd, &mut std::io::stdin().lock(), &mut std::io::stdout());
    let g = symgraph::load(cwd); // waits for the indexer if it is still running
    let syms: usize = g.files.values().map(|f| f.defs.len()).sum();
    println!("{}", paint(&format!("✓ graph ready: {} files, {syms} symbols ({:.1}s since start)\n", g.files.len(), started.elapsed().as_secs_f64()), MUTED));
}

pub fn main(args: Vec<String>) -> i32 {
    let cwd = args.get(0).map(std::path::PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let cwd = crate::util::paths::resolve(&cwd);
    let g = symgraph::load(&cwd);
    println!("Indexed {} source files in {}.", g.files.len(), cwd.display());
    interview(&cwd, &mut std::io::stdin().lock(), &mut std::io::stdout());
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_and_ignore_rules() {
        assert!(is_noise("web/node_modules/x/a.js") && is_noise("src/api.d.ts") && is_noise("pb/user.pb.go"));
        assert!(!is_noise("src/distance.ts") && !is_noise("src/app.ts"));
        let c = GraphCfg { ignore: vec!["examples/".into(), "*.stories.tsx".into()], ..Default::default() };
        assert!(c.is_ignored("examples/a.ts") && c.is_ignored("pkg/examples/a.ts") && c.is_ignored("ui/Button.stories.tsx"));
        assert!(!c.is_ignored("src/example_usage.ts") && !c.is_ignored("src/app.ts"));
    }

    #[test]
    fn interview_records_answers_and_graph_drops_ignored_folders() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        std::process::Command::new("git").args(["init", "-q"]).current_dir(p).status().unwrap();
        for dir in ["examples", "src/billing"] {
            std::fs::create_dir_all(p.join(dir)).unwrap();
            for i in 0..3 {
                std::fs::write(p.join(format!("{dir}/m{i}.py")), format!("def f{i}():\n    pass\n")).unwrap();
            }
        }
        std::fs::create_dir_all(p.join("node_modules/x")).unwrap();
        std::fs::write(p.join("node_modules/x/a.js"), "function junk() {}").unwrap();
        // examples/ -> Enter (default: leave out); src/ -> area note; src/billing/ is below the 8-file bar so is not asked
        let mut input = std::io::Cursor::new("\nInvoices and webhooks\n\n");
        let mut out = vec![];
        let n = interview(p, &mut input, &mut out);
        let text = String::from_utf8(out).unwrap();
        assert!(n >= 1 && text.contains("`examples/`"), "{text}");
        let cfg = load(p);
        assert_eq!(cfg.ignore, ["examples/"]);
        assert!(cfg.areas.values().any(|v| v == "Invoices and webhooks"), "{cfg:?}");
        // asking again does not repeat what was decided
        assert!(!questions(p, &cfg, 10).iter().any(|q| matches!(q, Q::Side { dir, .. } if dir == "examples")));
        let g = symgraph::load(p);
        assert!(g.find("junk", 5).is_empty(), "node_modules is dropped");
        assert!(g.find("f0", 5).iter().all(|(f, _)| !f.starts_with("examples/")), "ignored folder is gone from the graph");
        assert!(g.find("f0", 5).iter().any(|(f, _)| f.starts_with("src/billing/")));
    }
}
