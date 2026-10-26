//! Repository rules enforced in code. A prompt saying "never read .env" is a request; this makes it a boundary, for
//! the main agent, subagents and workers alike, because every tool call passes through ToolRegistry::invoke.
use super::workspace::Workspace;
use crate::config::Config;
use crate::util::fnmatch::fnmatch;
use crate::util::re::Re;
use serde_json::Value;
use std::path::Path;

const PATH_KEYS: &[&str] = &["path", "file_path", "cwd", "directory", "dir", "paths", "source", "destination"];
/// Templates document which secrets exist without holding any: .env.example stays readable while .env.prod is not.
const TEMPLATE_SUFFIXES: &[&str] = &[".example", ".sample", ".template", ".dist"];
/// Tools that only look: they may open skill files outside the workspace, nothing else.
const READ_TOOLS: &[&str] = &["read_file", "list_dir", "grep", "glob", "outline"];
const BUILTIN_TOOLS: &[&str] = &[
    "read_file", "write_file", "edit", "shell", "list_dir", "grep", "search", "outline", "git", "glob", "web_search", "web_fetch", "todos", "memory",
    "use_skill", "graph", "parallel_tasks", "ask_user", "notes", "advisor", "recall_result",
];

/// Matching pattern if any part of the path (file or parent dir name) hits.
pub fn is_forbidden_path(path: &str, patterns: &[String]) -> Option<String> {
    if patterns.is_empty() {
        return None;
    }
    for part in Path::new(path).components() {
        let low = part.as_os_str().to_string_lossy().to_lowercase();
        if TEMPLATE_SUFFIXES.iter().any(|s| low.ends_with(s)) {
            continue;
        }
        for pat in patterns {
            if fnmatch(&low, &pat.to_lowercase()) {
                return Some(pat.clone());
            }
        }
    }
    None
}

/// Words of a command, also split on redirections / pipes glued to words ("cat<.env") and on git's rev:path spelling.
fn tokens(command: &str) -> Vec<String> {
    let toks = shell_words::split(command).unwrap_or_else(|_| command.split_whitespace().map(String::from).collect());
    let mut out = vec![];
    for t in toks {
        out.extend(t.split(|c| "<>|;&=:".contains(c)).filter(|x| !x.is_empty()).map(String::from));
    }
    out
}

fn in_skill_dir(path: &str, config: &Config) -> bool {
    let p = crate::util::paths::expanduser(path);
    let p = if p.is_absolute() { p } else { config.cwd.join(p) };
    let real = crate::util::paths::resolve(&p);
    crate::agent::skills::skill_roots(config).iter().any(|r| real == *r || real.starts_with(r))
}

/// Why this call must not run, or None.
pub fn violation(tool_name: &str, params: &Value, config: &Config) -> Option<String> {
    let ws = Workspace::new(&config.cwd, &config.allowed_dirs);
    let obj = params.as_object()?;
    for (key, value) in obj {
        if !PATH_KEYS.contains(&key.as_str()) {
            continue;
        }
        let items: Vec<&Value> = match value {
            Value::Array(a) => a.iter().collect(),
            v => vec![v],
        };
        for v in items {
            let Some(v) = v.as_str() else { continue };
            if !ws.contains(v, None) && !(READ_TOOLS.contains(&tool_name) && in_skill_dir(v, config)) {
                return Some(format!(
                    "path '{v}' is outside the workspace ({}). Work inside the workspace, or ask the user to add the directory with --add-dir",
                    ws.root.display()
                ));
            }
            if let Some(hit) = is_forbidden_path(v, &config.forbidden_paths) {
                return Some(format!("path '{v}' matches protected pattern '{hit}'"));
            }
        }
    }

    if !BUILTIN_TOOLS.contains(&tool_name) && !tool_name.starts_with("subagent_") {
        // MCP and other external tools: scan every string argument for paths
        for v in obj.values() {
            if let Some(s) = v.as_str() {
                if let Some(why) = ws.shell_violation(s, None) {
                    return Some(why);
                }
            }
        }
    }

    if let Some(command) = obj.get("command").and_then(Value::as_str) {
        let cwd_arg = obj.get("cwd").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|c| ws.root.join(c));
        if let Some(mut why) = ws.shell_violation(command, cwd_arg.as_deref()) {
            let roots = crate::agent::skills::skill_roots(config);
            // the model reaches for `ls ~/.pi/agent/skills/x` in the shell: say where skill files can be read instead
            if roots.iter().any(|r| why.contains(&*r.to_string_lossy())) {
                why.push_str(". Skill files are readable with read_file, list_dir or grep, not the shell");
            }
            return Some(why);
        }
        for pat in &config.forbidden_commands {
            if Re::new(pat, true).map_or(false, |r| r.is_match(command)) {
                return Some(format!("command matches forbidden pattern '{pat}'"));
            }
        }
        for tok in tokens(command) {
            if let Some(hit) = is_forbidden_path(&tok, &config.forbidden_paths) {
                return Some(format!("command touches '{tok}' (protected pattern '{hit}')"));
            }
        }
    }
    None
}
