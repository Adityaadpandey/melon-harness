//! Finds and merges config: the user's `config.toml`, the project's `.melon/config.toml` (gated keys need trust),
//! a repository profile, and the AGENTS.md / CLAUDE.md instruction files.
use super::trust::{is_trusted, trust, GATED_KEYS};
use super::Config;
use crate::util::paths::resolve;
use crate::util::re::{lit, lit_i, Re};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const INSTRUCTION_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", "AGENT.MD"];
/// Claude Code loads a whole CLAUDE.md. It sits in the cached prefix, so after the first call it costs almost nothing,
/// and an outline alone hid every rule from the model. ponytail: one fixed cap; past it a rulebook falls back to the outline.
pub const INLINE_LIMIT: usize = 90_000;

/// `~/Library/Application Support/melon` on macOS, `~/.config/melon` elsewhere (platformdirs' `user_config_dir`).
pub fn get_config_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        dirs::home_dir().unwrap_or_default().join("Library/Application Support/melon")
    } else {
        dirs::config_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config")).join("melon")
    }
}

pub fn get_data_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        dirs::home_dir().unwrap_or_default().join("Library/Application Support/melon")
    } else {
        dirs::data_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/share")).join("melon")
    }
}

/// The folder holding profiles/ and skills/: next to the binary's source tree, or `MELON_HOME`.
pub fn home_dir() -> PathBuf {
    if let Ok(h) = std::env::var("MELON_HOME") {
        return PathBuf::from(h);
    }
    if let Ok(exe) = std::env::current_exe() {
        for a in exe.ancestors().skip(1).take(5) {
            if a.join("profiles").is_dir() && a.join("skills").is_dir() {
                return a.to_path_buf();
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_default()
}

pub fn save_last_model(name: &str) {
    let d = get_data_dir();
    if std::fs::create_dir_all(&d).is_ok() {
        let _ = std::fs::write(d.join("last_model"), name.trim());
    }
}

pub fn load_last_model() -> Option<String> {
    std::fs::read_to_string(get_data_dir().join("last_model")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Keep an API key in melon's own .env (loaded at every start), readable only by you.
pub fn save_env_key(name: &str, value: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = get_data_dir().join(".env");
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().filter(|l| !l.starts_with(&format!("{name}="))).map(String::from).collect();
    lines.push(format!("{name}={value}"));
    std::fs::write(&path, lines.join("\n") + "\n")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    unsafe { std::env::set_var(name, value) };
    Ok(path)
}

/// Minimal .env reader: `KEY=value` lines, optional quotes, `#` comments. Never overrides a variable already set.
pub fn load_dotenv(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            let v = if (v.starts_with('"') && v.ends_with('"') || v.starts_with('\'') && v.ends_with('\'')) && v.len() >= 2 { &v[1..v.len() - 1] } else { v };
            if std::env::var_os(k.trim()).is_none() {
                unsafe { std::env::set_var(k.trim(), v) };
            }
        }
    }
}

pub fn get_system_config_path() -> PathBuf {
    get_config_dir().join(CONFIG_FILE_NAME)
}

pub fn toml_to_json(v: toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => Value::from(i),
        toml::Value::Float(f) => Value::from(f),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => Value::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect()),
    }
}

pub fn parse_toml(path: &Path) -> Result<Map<String, Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Failed to read config file {}: {e}", path.display()))?;
    let t: toml::Table = text.parse().map_err(|e| format!("Invalid TOML in {}: {e}", path.display()))?;
    match toml_to_json(toml::Value::Table(t)) {
        Value::Object(m) => Ok(m),
        _ => Ok(Map::new()),
    }
}

fn project_config(cwd: &Path) -> Option<PathBuf> {
    let f = resolve(cwd).join(".melon").join(CONFIG_FILE_NAME);
    f.is_file().then_some(f)
}

fn outline(text: &str, max_lines: usize) -> String {
    text.lines()
        .enumerate()
        .filter(|(_, l)| l.starts_with('#') && !l.starts_with("####"))
        .map(|(i, l)| format!("  line {}: {}", i + 1, l.trim()))
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Inline `@other.md` lines (Claude Code style), one level deep, files next to f only.
fn with_imports(f: &Path, text: &str) -> String {
    let re = lit_i(r"^@(\S+\.md)\s*$");
    text.split('\n')
        .map(|line| match re.captures(line) {
            Some(c) => {
                let name = c[1].clone().unwrap_or_default();
                let target = f.parent().unwrap_or(Path::new(".")).join(&name);
                if target.is_file() && target != f {
                    std::fs::read_to_string(&target).unwrap_or_else(|_| line.to_string())
                } else {
                    line.to_string()
                }
            }
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lines of a long rulebook about commits, comments, specs and plans, which must never be skipped.
fn house_rules(text: &str, limit: usize) -> String {
    let re = lit_i(r"commit|comment|\bspec\b|\bplan\b|ask before");
    let hits: Vec<&str> = text.lines().filter(|l| re.is_match(l)).map(str::trim).collect();
    hits.join("\n").chars().take(limit).collect()
}

fn taste_head() -> Re {
    Re::new(r"(?m)^# Taste \(Continuously Learned.*?\n|^\[cmd\]:.*\n|^See \[[^\]]*\](\([^)]*\))?\s*\n", false).unwrap()
}

/// Command Code's learned preferences in a folder: .commandcode/taste/taste.md plus one per topic.
pub fn taste_files(d: &Path) -> Vec<PathBuf> {
    let t = d.join(".commandcode").join("taste");
    let mut out = vec![];
    if t.join("taste.md").is_file() {
        out.push(t.join("taste.md"));
    }
    let mut subs: Vec<PathBuf> = std::fs::read_dir(&t)
        .map(|rd| rd.flatten().map(|e| e.path().join("taste.md")).filter(|p| p.is_file()).collect())
        .unwrap_or_default();
    subs.sort();
    out.extend(subs);
    out
}

pub fn taste_text(f: &Path) -> String {
    let Ok(raw) = std::fs::read_to_string(f) else { return String::new() };
    // the header lines are matched one at a time; the regex crate has no lazy `.*?` anchored multi-alternation issue here
    let mut out = String::new();
    let re = taste_head();
    for line in raw.split_inclusive('\n') {
        if re.is_match(line) {
            continue;
        }
        out.push_str(line);
    }
    out.trim().to_string()
}

fn taste_for(dirs: &[PathBuf], home: &Path) -> Option<String> {
    let mut blocks = vec![];
    let mut all: Vec<PathBuf> = dirs.to_vec();
    all.push(home.to_path_buf());
    all.push(home.join(".melon"));
    for d in all {
        for f in taste_files(&d) {
            let t = taste_text(&f);
            if !t.is_empty() {
                blocks.push(format!("### {}\n{t}", f.display()));
            }
        }
    }
    if blocks.is_empty() {
        return None;
    }
    Some(format!(
        "## Learned taste (remembered preferences, each with a confidence; follow them by default, but the user's request and the instruction files above win when they disagree)\n\n{}",
        blocks.join("\n\n")
    ))
}

/// AGENTS.md / CLAUDE.md from cwd up to the repo root, closest first, then the user's own ~/.claude/CLAUDE.md.
/// Every file found is used, not just one per directory. Short files go into the prompt as-is; a big rulebook
/// contributes its headings and the model reads the sections it needs.
pub fn instructions_for(cwd: &Path, home: Option<&Path>) -> Option<String> {
    let mut blocks: Vec<String> = vec![];
    let mut seen: Vec<PathBuf> = vec![];
    let mut texts: Vec<String> = vec![];
    let mut dirs: Vec<PathBuf> = vec![];
    let c = resolve(cwd);
    for d in c.ancestors().take(8) {
        dirs.push(d.to_path_buf());
        if d.join(".git").exists() {
            break;
        }
    }
    let repo_dirs: Vec<PathBuf> = dirs.iter().rev().cloned().collect();
    let home = home.map(Path::to_path_buf).unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
    dirs.push(home.join(".claude"));
    for d in &dirs {
        for name in INSTRUCTION_FILES {
            let f = d.join(name);
            if !f.is_file() {
                continue;
            }
            let rf = resolve(&f);
            if seen.contains(&rf) {
                continue;
            }
            let raw = std::fs::read_to_string(&f).unwrap_or_default();
            let text = with_imports(&f, &raw);
            if text.trim().chars().count() < 40 || texts.iter().any(|t| *t == text.trim()) {
                continue; // a stub like "@AGENTS.md", or a copy
            }
            seen.push(rf);
            texts.push(text.trim().to_string());
            if text.chars().count() <= INLINE_LIMIT {
                blocks.push(format!("## {}\n{text}", f.display()));
            } else {
                blocks.push(format!(
                    "## {} ({} chars, too long to inline)\nRead the sections relevant to your task with read_file (offset/limit) BEFORE editing. Section map:\n{}\nRules that always apply (commits, comments, specs, plans):\n{}",
                    f.display(),
                    text.chars().count(),
                    outline(&text, 100),
                    house_rules(&text, 1500)
                ));
            }
        }
    }
    if let Some(t) = taste_for(&repo_dirs, &home) {
        blocks.push(t);
    }
    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

/// Repository profile: extra rules for a known project, picked by name or by a `match` entry equal to a component of cwd.
fn load_profile(name: Option<&str>, cwd: &Path) -> Option<Map<String, Value>> {
    let dir = home_dir().join("profiles");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "toml")).collect();
    files.sort();
    let parts: Vec<String> = resolve(cwd).components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect();
    for f in files {
        let Ok(mut data) = parse_toml(&f) else { continue };
        let stem = f.file_stem()?.to_string_lossy().to_string();
        let matches = data.get("match").and_then(|m| m.as_array()).map_or(false, |a| a.iter().any(|m| m.as_str().map_or(false, |s| parts.contains(&s.to_lowercase()))));
        if name.map_or(false, |n| n == stem) || (name.is_none() && matches) {
            data.insert("profile".into(), Value::String(stem));
            return Some(data);
        }
    }
    None
}

pub fn merge(base: &mut Map<String, Value>, over: Map<String, Value>) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(Value::Object(b)), Value::Object(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

pub struct LoadOptions {
    pub trust_project: bool,
    pub pi_mode: Option<bool>,
}

pub fn load_config(cwd: Option<&Path>, opts: &LoadOptions) -> Result<Config, String> {
    let cwd = cwd.map(Path::to_path_buf).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    // Only the harness's own .env (where API_KEY lives). The target repo's .env is never opened: it holds that
    // project's secrets, and loading it would also hand them to every shell command the agent runs.
    load_dotenv(&home_dir().join(".env"));
    load_dotenv(&get_data_dir().join(".env"));

    let mut dict = Map::new();
    let sys = get_system_config_path();
    if sys.is_file() {
        match parse_toml(&sys) {
            Ok(m) => dict = m,
            Err(e) => eprintln!("Skipping invalid system config: {e}"),
        }
    }
    let mut ignored: Vec<String> = vec![];
    if let Some(pp) = project_config(&cwd) {
        match parse_toml(&pp) {
            Ok(mut project) => {
                let mut gated: Vec<String> = project.keys().filter(|k| GATED_KEYS.contains(&k.as_str())).cloned().collect();
                gated.sort();
                let data = get_data_dir();
                if !gated.is_empty() && !(opts.trust_project || is_trusted(&cwd, &pp, &data)) {
                    for k in &gated {
                        project.remove(k);
                    }
                    ignored = gated;
                } else if !gated.is_empty() && opts.trust_project {
                    trust(&cwd, &pp, &data);
                }
                merge(&mut dict, project);
            }
            Err(e) => eprintln!("Skipping invalid project config: {e}"),
        }
    }
    dict.entry("cwd").or_insert_with(|| Value::String(cwd.to_string_lossy().to_string()));
    if let Some(p) = opts.pi_mode {
        // --pi / --no-pi beat every config file; set here so pi's defaults fill in (or don't) to match
        dict.insert("pi_mode".into(), Value::Bool(p));
    }
    let profile = load_profile(dict.get("profile").and_then(Value::as_str), &cwd);
    let mut profile_text = None;
    if let Some(mut pr) = profile {
        profile_text = pr.remove("instructions").and_then(|v| v.as_str().map(String::from));
        for k in ["match", "name", "version"] {
            pr.remove(k);
        }
        let mut merged = pr;
        merge(&mut merged, dict);
        dict = merged; // project config wins
    }
    if !dict.contains_key("developer_instructions") {
        let parts: Vec<String> = [profile_text, instructions_for(&cwd, None)].into_iter().flatten().collect();
        if !parts.is_empty() {
            dict.insert("developer_instructions".into(), Value::String(parts.join("\n\n")));
        }
    }
    let mut cfg = Config::from_map(dict).map_err(|e| format!("Invalid configuration: {e}"))?;
    cfg.ignored_project_keys = ignored;
    let _ = (lit as fn(&str) -> Re,);
    Ok(cfg)
}
