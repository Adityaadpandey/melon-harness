//! Prompt templates: a markdown file in a prompts folder becomes a /command.
//!     ~/.pi/agent/prompts/review.md      ->  /review  or  /review concurrency
//! The body is sent as the user's message after the arguments are filled in:
//!     $1 $2      one argument            ${1:-default}  first argument or a default
//!     $@         all arguments           ${@:2}         arguments from the second on
//! Quote to keep spaces together: /review "API compatibility".
use crate::util::re::{lit, Re};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub fn prompt_dirs(cwd: Option<&Path>) -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs = vec![home.join(".pi/agent/prompts"), home.join(".melon/prompts"), home.join(".claude/commands"), home.join(".melon/commands")];
    if let Some(c) = cwd {
        dirs.push(c.join(".claude/commands"));
        dirs.push(c.join(".melon/commands"));
    }
    dirs
}

fn walk_md(d: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk_md(&p, out);
        } else if p.extension().map_or(false, |e| e == "md") {
            out.push(p);
        }
    }
}

/// name -> (body, description). `review/security.md` is named `review:security`, as in Claude Code.
pub fn load_commands(dirs: &[PathBuf]) -> BTreeMap<String, (String, String)> {
    let mut found = BTreeMap::new();
    for d in dirs.iter().filter(|d| d.is_dir()) {
        let mut files = vec![];
        walk_md(d, &mut files);
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let (meta, body) = match text.strip_prefix("---\n").and_then(|t| t.split_once("\n---\n")) {
                Some((m, b)) => (m.to_string(), b.trim().to_string()),
                None => (String::new(), text.trim().to_string()),
            };
            let desc = meta
                .lines()
                .filter(|l| !l.starts_with(char::is_whitespace))
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| *k == "description")
                .map(|(_, v)| v.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| body.lines().find(|l| !l.trim().is_empty()).map(|l| l.trim_matches(|c: char| c == ' ' || c == '#').to_string()))
                .unwrap_or_default();
            let rel = f.strip_prefix(d).unwrap_or(&f).with_extension("");
            let name = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join(":");
            found.insert(name, (body, desc.chars().take(100).collect()));
        }
    }
    found
}

pub fn split_args(text: &str) -> Vec<String> {
    shell_words::split(text).unwrap_or_else(|_| text.split_whitespace().map(String::from).collect())
}

fn arg_re() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"\$\{(@|\d+)(?::(\d+)|:-([^}]*))?\}|\$(@|ARGUMENTS|\d+)"))
}

pub fn expand(body: &str, args: &[String]) -> String {
    let re = arg_re();
    let mut out = String::new();
    let mut at = 0;
    while let Some(c) = re.captures(&body[at..]) {
        let (s, e) = re.find(&body[at..]).unwrap();
        out.push_str(&body[at..at + s]);
        let which = c[1].clone().or(c[4].clone()).unwrap_or_default();
        let picked: Vec<String> = if let Some(Some(start)) = c.get(2) {
            let n: usize = start.parse().unwrap_or(1);
            args.iter().skip(n.saturating_sub(1)).cloned().collect()
        } else if which == "@" || which == "ARGUMENTS" {
            args.to_vec()
        } else {
            let n: usize = which.parse().unwrap_or(1);
            args.get(n.saturating_sub(1)).cloned().into_iter().collect()
        };
        if picked.is_empty() {
            out.push_str(&c.get(3).cloned().flatten().unwrap_or_default());
        } else {
            out.push_str(&picked.join(" "));
        }
        at += e.max(1);
    }
    out.push_str(&body[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fills() {
        let a = split_args("one \"two words\" three");
        assert_eq!(expand("$1|$2|${3:-x}|${4:-dflt}|$@|${@:2}", &a), "one|two words|three|dflt|one two words three|two words three");
    }
}
