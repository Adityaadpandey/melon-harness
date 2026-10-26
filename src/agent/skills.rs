//! Skills: curated playbooks the agent can load instead of rediscovering a project's rules every run. A skill is a
//! markdown file with a small header (name, description, triggers, canonical, always) and a short body. `canonical`
//! points at the full document in the repo; the agent pulls single sections of it on demand (use_skill with `section`).
use crate::config::loader::home_dir;
use crate::config::Config;
use crate::util::paths::resolve;
use crate::util::re::{lit, Re};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const SECTION_CAP: usize = 14_000;
/// Total chars of playbooks auto-injected per request.
const AUTO_LOAD_CAP: usize = 16_000;

#[derive(Clone, Debug, Default)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub triggers: Vec<String>,
    pub canonical: Option<String>,
    pub path: Option<PathBuf>,
    /// Inlined into the system prompt instead of waiting to be triggered.
    pub always: bool,
}

impl Skill {
    /// A phrase counts for its words: "review the commits" is a stronger hint than the bare word "commit".
    pub fn score(&self, text: &str) -> usize {
        let low = text.to_lowercase();
        self.triggers
            .iter()
            .filter(|t| Re::new(&format!(r"(?<![\w]){}", regex::escape(&t.to_lowercase())), false).map_or(false, |r| r.is_match(&low)))
            .map(|t| t.split_whitespace().count())
            .sum()
    }
}

/// Skill folders shared with other agents, as (below melon's built-ins, above them). Claude Code's (plugins, then
/// ~/.claude/skills) rank below, so melon's compact always-on ponytail beats the plugin's longer one; pi's folder ranks
/// above, and ~/.melon/skills (the user's own pick for melon) ranks above all.
fn user_dirs() -> (Vec<PathBuf>, Vec<PathBuf>) {
    let home = dirs::home_dir().unwrap_or_default();
    let mut below = vec![];
    if let Some(v) = std::fs::read_to_string(home.join(".claude/plugins/installed_plugins.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        if let Some(plugins) = v["plugins"].as_object() {
            for (_, installs) in plugins {
                if let Some(p) = installs.as_array().and_then(|a| a.last()).and_then(|i| i["installPath"].as_str()) {
                    below.push(PathBuf::from(p).join("skills")); // the newest install of each
                }
            }
        }
    }
    below.push(home.join(".claude/skills"));
    (below, vec![home.join(".pi/agent/skills"), home.join(".melon/skills")])
}

pub fn parse_skill(text: &str, path: Option<&Path>) -> Option<Skill> {
    let rest = text.strip_prefix("---\n")?;
    let (head, body) = rest.split_once("\n---\n")?;
    let lines: Vec<&str> = head.lines().collect();
    let mut meta: HashMap<String, String> = HashMap::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some((k, v)) = line.split_once(':') {
            if line.starts_with(char::is_whitespace) {
                continue;
            }
            let mut v = v.split("  #").next().unwrap_or("").trim().to_string();
            if matches!(v.as_str(), "|" | ">" | "|-" | ">-") {
                // a YAML block: the value is the indented lines below it
                v = lines[i + 1..].iter().take_while(|x| x.is_empty() || x.starts_with(char::is_whitespace)).map(|x| x.trim()).collect::<Vec<_>>().join(" ");
            }
            meta.insert(k.trim().to_string(), v.trim().trim_matches(|c| c == '\'' || c == '"').to_string());
        }
    }
    let name = meta.get("name")?.clone();
    let triggers = meta.get("triggers").or(meta.get("trigger")).map(|t| t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
    Some(Skill {
        name,
        description: meta.get("description").cloned().unwrap_or_default(),
        body: body.trim().to_string(),
        triggers,
        canonical: meta.get("canonical").filter(|c| !c.is_empty()).cloned(),
        path: path.map(Path::to_path_buf),
        always: meta.get("always").map_or(false, |a| a.to_lowercase() == "true"),
    })
}

type Key = (Option<String>, String, Vec<String>);

fn load_uncached(profile: Option<&str>, cwd: &str, skill_dirs: &[String]) -> Vec<Skill> {
    let (shared, pi) = user_dirs();
    let mut dirs: Vec<PathBuf> = shared.clone();
    dirs.push(home_dir().join("skills")); // ship with the harness, apply to every project
    dirs.extend(pi);
    if let Some(p) = profile {
        dirs.push(home_dir().join("profiles").join(p).join("skills"));
    }
    let root = Path::new(cwd);
    dirs.extend([root.join(".claude/skills"), root.join(".melon/skills")]);
    dirs.extend(skill_dirs.iter().map(|d| root.join(d)));
    let mut found: Vec<Skill> = vec![];
    let quoted = lit(r#""([^"]{4,40})""#);
    for d in &dirs {
        if !d.is_dir() {
            continue;
        }
        // flat files (skills/debugging.md) and pi-style folders (skills/review-pr/SKILL.md)
        let mut files: Vec<PathBuf> = std::fs::read_dir(d).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        let mut expanded = vec![];
        files.sort();
        for f in files {
            if f.is_dir() {
                let s = f.join("SKILL.md");
                if s.is_file() {
                    expanded.push(s);
                }
            } else if f.extension().map_or(false, |e| e == "md") {
                expanded.push(f);
            }
        }
        expanded.sort_by(|a, b| {
            // the Python glob order: flat *.md first, then */SKILL.md
            let a_skill = a.file_name().map_or(false, |n| n == "SKILL.md");
            let b_skill = b.file_name().map_or(false, |n| n == "SKILL.md");
            a_skill.cmp(&b_skill).then(a.cmp(b))
        });
        for f in expanded {
            let Ok(text) = std::fs::read(&f).map(|b| String::from_utf8_lossy(&b).to_string()) else { continue }; // a broken symlink is skipped, not fatal
            let Some(mut sk) = parse_skill(&text, Some(&f)) else { continue };
            // pi skills carry no trigger words, so their name is the hint. Claude Code ones are only listed: names like
            // "review" or "ship" would fire on half of all requests.
            if sk.triggers.is_empty() && f.file_name().map_or(false, |n| n == "SKILL.md") && !sk.always && !shared.contains(d) {
                let mut t = vec![sk.name.clone(), sk.name.replace('-', " ")];
                let mut at = 0;
                while let Some(c) = quoted.captures(&sk.description[at..]) {
                    let (_, e) = quoted.find(&sk.description[at..]).unwrap();
                    t.extend(c[1].clone());
                    at += e.max(1);
                }
                sk.triggers = t;
            }
            found.retain(|x| x.name != sk.name);
            found.push(sk); // later dirs (repo-local) override profile ones
        }
    }
    if let Some(p) = profile {
        // a profile's own playbook (e.g. tunyt-debugging) replaces the generic one it specialises
        let names: Vec<String> = found.iter().map(|s| s.name.clone()).collect();
        found.retain(|s| !names.contains(&format!("{p}-{}", s.name)));
    }
    found
}

pub fn load_skills(config: &Config) -> Vec<Skill> {
    static CACHE: OnceLock<Mutex<HashMap<Key, Vec<Skill>>>> = OnceLock::new();
    let key: Key = (config.profile.clone(), config.cwd.to_string_lossy().to_string(), config.skill_dirs.clone());
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let skills = {
        let mut c = cache.lock().unwrap();
        c.entry(key.clone()).or_insert_with(|| load_uncached(key.0.as_deref(), &key.1, &key.2)).clone()
    };
    skills.into_iter().filter(|s| !config.disabled_skills.contains(&s.name)).collect()
}

/// Folders the model may read skill files from, wherever the repo is: each folder skill's own folder (its references
/// come along) and the folders holding flat skill files.
pub fn skill_roots(config: &Config) -> Vec<PathBuf> {
    load_skills(config).iter().filter_map(|s| s.path.as_ref().and_then(|p| p.parent()).map(resolve)).collect::<BTreeSet<_>>().into_iter().collect()
}

/// Deterministic keyword routing: no model call, so it is free, instant and testable.
pub fn match_skills(skills: &[Skill], request: &str, limit: usize) -> Vec<Skill> {
    let mut scored: Vec<(usize, &Skill)> = skills.iter().filter(|s| !s.always).map(|s| (s.score(request), s)).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let (mut picked, mut used): (Vec<Skill>, usize) = (vec![], 0);
    for (score, s) in scored {
        if score < 1 || picked.len() >= limit {
            break;
        }
        if used + s.body.len() > AUTO_LOAD_CAP && !picked.is_empty() {
            continue;
        }
        used += s.body.len();
        picked.push(s.clone());
    }
    picked
}

/// (line number, level, title) for every markdown heading outside code fences.
pub fn headings(text: &str) -> Vec<(usize, usize, String)> {
    let re = lit(r"^(#{1,4})\s+(.*)");
    let (mut out, mut fenced) = (vec![], false);
    for (i, line) in text.lines().enumerate() {
        if line.starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            if let Some(c) = re.captures(line) {
                out.push((i + 1, c[1].as_ref().map_or(0, |s| s.len()), c[2].clone().unwrap_or_default().trim().to_string()));
            }
        }
    }
    out
}

pub fn extract_section(text: &str, query: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let hs = headings(text);
    let q = query.to_lowercase();
    for (idx, (ln, level, title)) in hs.iter().enumerate() {
        if title.to_lowercase().contains(&q) {
            let end = hs[idx + 1..].iter().find(|(_, l2, _)| l2 <= level).map_or(lines.len(), |(l, _, _)| l - 1);
            return Some(lines[ln - 1..end].join("\n"));
        }
    }
    None
}

/// Canonical docs are repo-root relative; the agent may be running in a subfolder.
pub fn find_in_repo(cwd: &Path, rel: &str) -> Option<PathBuf> {
    resolve(cwd).ancestors().map(|d| d.join(rel)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_and_match() {
        let s = parse_skill("---\nname: debugging\ndescription: find bugs\ntriggers: bug, stuck job\nalways: false\n---\nBody here\n", None).unwrap();
        assert_eq!(s.name, "debugging");
        assert_eq!(s.triggers, vec!["bug", "stuck job"]);
        assert_eq!(s.score("there is a stuck job and a bug"), 3);
        let m = match_skills(&[s], "fix this bug", 4);
        assert_eq!(m.len(), 1);
        assert!(parse_skill("no header", None).is_none());
    }
    #[test]
    fn sections() {
        let doc = "# A\nintro\n## B\nb text\n### C\nc\n## D\nd\n";
        assert_eq!(extract_section(doc, "b").unwrap(), "## B\nb text\n### C\nc");
        assert!(extract_section(doc, "zzz").is_none());
    }
}
