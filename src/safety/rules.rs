//! Allow / deny / ask rules in Claude Code's notation, so a project can say what Melon may do without a prompt.
//!
//!     Bash(git diff:*)     any command starting with `git diff`
//!     Bash(npm run build)  exactly that command
//!     Edit(src/**)         edits (and new files) anywhere under src/
//!     Read(.env*)          reading, grepping or listing anything named .env...
//!     WebFetch(domain:docs.python.org)
//!     Bash                 the whole tool (no specifier)
//!
//! Example `.melon/permissions.json`:  {"allow": ["Bash(pnpm test:*)"], "deny": ["Read(.env*)", "Bash(rm:*)"], "ask": ["Bash(git push:*)"]}
//! Deny wins over ask, ask over allow. An allow never lifts the built-in refusals (plan mode, dangerous commands).
use super::approval::split_pipeline;
use crate::util::fnmatch::fnmatch;
use crate::util::paths::resolve;
use crate::util::re::{lit, Re};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const KINDS: [&str; 3] = ["deny", "ask", "allow"];

fn aliases(tool: &str) -> Vec<&str> {
    match tool.to_lowercase().as_str() {
        "bash" => vec!["shell"],
        "edit" => vec!["edit", "write_file"],
        "write" => vec!["write_file"],
        "read" => vec!["read_file", "grep", "glob", "list_dir", "search", "outline"],
        "webfetch" => vec!["web_fetch"],
        "websearch" => vec!["web_search"],
        _ => vec![],
    }
}

/// Gitignore-flavoured: `src/**` everything under src, `*.key` that name at any depth, `docs/*.md` one level only.
fn glob_re(spec: &str) -> Re {
    let spec = spec.strip_prefix("./").unwrap_or(spec);
    let anchored = spec.starts_with('/') || spec.trim_end_matches('/').contains('/');
    let spec = spec.trim_start_matches('/');
    let b = spec.as_bytes();
    let (mut body, mut i) = (String::new(), 0);
    while i < b.len() {
        if spec[i..].starts_with("**/") {
            body.push_str("(?:.*/)?");
            i += 3;
        } else if spec[i..].starts_with("**") {
            body.push_str(".*");
            i += 2;
        } else if b[i] == b'*' {
            body.push_str("[^/]*");
            i += 1;
        } else if b[i] == b'?' {
            body.push_str("[^/]");
            i += 1;
        } else {
            let ch = spec[i..].chars().next().unwrap();
            body.push_str(&regex::escape(&ch.to_string()));
            i += ch.len_utf8();
        }
    }
    if spec.ends_with('/') {
        body.push_str(".*");
    }
    let pat = if anchored { format!("^(?s:{body})$") } else { format!("^(?s:(?:.*/)?{body}(?:/.*)?)$") };
    lit(&pat)
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub kind: String,
    pub tool: String,
    pub spec: Option<String>,
    pub text: String,
    pub source: String,
}

impl Rule {
    fn covers_tool(&self, name: &str) -> bool {
        aliases(&self.tool).contains(&name) || name == self.tool
    }

    fn path_matches(&self, target: &str, cwd: &Path) -> bool {
        let t = resolve(Path::new(target));
        let rel = t.strip_prefix(resolve(cwd)).map(|r| r.to_string_lossy().to_string()).unwrap_or_else(|_| target.to_string());
        glob_re(self.spec.as_deref().unwrap_or("*")).is_match(&rel)
    }

    fn command_matches(&self, command: &str) -> bool {
        let spec = self.spec.as_deref().unwrap_or("").trim();
        if let Some(head) = spec.strip_suffix(":*") {
            let head = head.trim();
            return command == head || command.starts_with(&format!("{head} "));
        }
        if spec.contains('*') {
            return fnmatch(command, spec);
        }
        command == spec
    }

    pub fn matches(&self, name: &str, params: &Value, cwd: &Path) -> bool {
        if !self.covers_tool(name) {
            return false;
        }
        let Some(spec) = &self.spec else { return true };
        if name == "shell" {
            let command = params["command"].as_str().unwrap_or("").trim().to_string();
            return match split_pipeline(&command) {
                // a line that does more than chain commands: judge the whole text
                None => self.command_matches(&command) || (self.kind != "allow" && command.contains(spec.strip_suffix(":*").unwrap_or(spec))),
                Some(parts) => {
                    let joined: Vec<String> = parts.iter().map(|w| w.join(" ")).collect();
                    // deny / ask fire when ANY piece matches; allow only when EVERY piece does (`ls && rm x` is not "allowed" by Bash(ls))
                    if self.kind == "allow" { joined.iter().all(|c| self.command_matches(c)) } else { joined.iter().any(|c| self.command_matches(c)) }
                }
            };
        }
        if name == "web_fetch" {
            let url = params["url"].as_str().unwrap_or("");
            let host = url.split("://").nth(1).unwrap_or(url).split(['/', ':', '?']).next().unwrap_or("").to_string();
            let want = spec.strip_prefix("domain:").unwrap_or(spec);
            return host == want || host.ends_with(&format!(".{want}"));
        }
        match params["path"].as_str() {
            Some(t) => self.path_matches(t, cwd),
            None => false,
        }
    }
}

pub fn parse_rule(text: &str, kind: &str, source: &str) -> Option<Rule> {
    static RULE: std::sync::OnceLock<Re> = std::sync::OnceLock::new();
    let re = RULE.get_or_init(|| lit(r"(?s)^\s*([\w.\-]+)\s*(?:\((.*)\))?\s*$"));
    if !KINDS.contains(&kind) {
        return None;
    }
    let c = re.captures(text)?;
    let spec = c.get(2).cloned().flatten().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Some(Rule { kind: kind.into(), tool: c[1].clone()?, spec, text: text.trim().into(), source: source.into() })
}

#[derive(Default, Clone)]
pub struct Rules {
    pub rules: Vec<Rule>,
    pub cwd: PathBuf,
}

impl Rules {
    /// The rule that applies to this call (deny first, then ask, then allow), or None.
    pub fn decide(&self, name: &str, params: &Value) -> Option<&Rule> {
        for kind in KINDS {
            for r in &self.rules {
                if r.kind == kind && r.matches(name, params, &self.cwd) {
                    return Some(r);
                }
            }
        }
        None
    }
}

fn read(path: &Path) -> Value {
    let Ok(text) = std::fs::read_to_string(path) else { return Value::Null };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return Value::Null };
    match v.get("permissions") {
        Some(p) => p.clone(),
        None => v,
    }
}

/// Where rules come from, loosest first. Claude Code's per-project settings are honoured so the two agents agree;
/// its global settings are not (they were written for a different tool's habits).
pub fn files(cwd: &Path) -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    vec![
        home.join(".melon/permissions.json"),
        cwd.join(".claude/settings.json"),
        cwd.join(".claude/settings.local.json"),
        cwd.join(".melon/permissions.json"),
    ]
}

pub fn load_rules(cwd: &Path) -> Rules {
    let mut out = vec![];
    for f in files(cwd) {
        let block = read(&f);
        for kind in KINDS {
            if let Some(items) = block.get(kind).and_then(Value::as_array) {
                for t in items.iter().filter_map(Value::as_str) {
                    if let Some(r) = parse_rule(t, kind, &f.to_string_lossy()) {
                        out.push(r);
                    }
                }
            }
        }
    }
    Rules { rules: out, cwd: cwd.to_path_buf() }
}

/// Append to the project's own file; other keys in it are left as they are.
pub fn save_rule(cwd: &Path, kind: &str, text: &str) -> std::io::Result<PathBuf> {
    let path = cwd.join(".melon/permissions.json");
    let mut data = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(Value::is_object).unwrap_or_else(|| serde_json::json!({}));
    let bucket = data.as_object_mut().unwrap().entry(kind).or_insert_with(|| serde_json::json!([]));
    if let Some(a) = bucket.as_array_mut() {
        if !a.iter().any(|x| x == text) {
            a.push(Value::String(text.into()));
        }
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&data).unwrap_or_default() + "\n")?;
    Ok(path)
}

pub fn drop_rule(cwd: &Path, text: &str) -> bool {
    let path = cwd.join(".melon/permissions.json");
    let Some(mut data) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return false };
    let mut hit = false;
    for kind in KINDS {
        if let Some(a) = data.get_mut(kind).and_then(Value::as_array_mut) {
            let before = a.len();
            a.retain(|x| x != text);
            hit |= a.len() != before;
        }
    }
    if hit {
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&data).unwrap_or_default() + "\n");
    }
    hit
}
