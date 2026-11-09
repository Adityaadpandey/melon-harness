//! First time melon opens a repo, write `.melon/config.toml` listing which model does which job, so changing one is
//! "uncomment a line" instead of digging through docs. Every line starts commented out, so the file changes nothing until
//! the developer edits it; an existing config file is never touched.
//!
//!     # fast = ["deepseek/deepseek-v4.1-flash"]   # small, easy requests
use super::Config;
use std::path::PathBuf;

fn list(models: &[String]) -> String {
    format!("[{}]", models.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join(", "))
}

/// One commented line: the models a role really uses today (or `[]` and why it has none), then what the role is for.
fn role_line(cfg: &Config, role: &str, fallback: &str, what: &str) -> String {
    let set = cfg.routing.get(role).filter(|c| !c.is_empty());
    let value = set.map_or_else(|| "[]".to_string(), |c| list(c));
    let note = if set.is_some() { what.to_string() } else { format!("{what} (not set: {fallback})") };
    format!("# {role:<9} = {value:<44} # {note}\n")
}

pub fn text(cfg: &Config) -> String {
    let mut c = cfg.clone();
    // roles that have a built-in model when nothing is configured
    if c.advisor && !c.routing.contains_key("advisor") {
        c.routing.insert("advisor".into(), vec![c.advisor_model.clone()]);
    }
    if !c.routing.contains_key("challenge") {
        c.routing.insert("challenge".into(), vec![crate::agent::challenge::CHALLENGER.into()]);
    }
    let mut s = String::from(
        "# Which model melon uses for which job in this repo.\n\
         # Every line is commented out, so nothing here changes anything yet.\n\
         # To change one: delete the leading \"# \" on the line (and on the [model] / [routing] header above it).\n\
         # A list is tried left to right: the first model works, the others are fallbacks if it fails.\n\n",
    );
    s += &format!("# [model]\n# name = {:?}   # main model: does the real work on every request\n\n# [routing]\n", cfg.model_name());
    s += &role_line(&c, "fast", "easy requests use the main model", "small, easy requests");
    s += &role_line(&c, "strong", "hard requests use the main model", "hard requests, and after a fix fails");
    s += &role_line(&c, "explore", "scout uses the main model", "scout: searches the repo first so the main model starts from findings");
    s += &role_line(&c, "summarize", "uses the fast model", "shrinks old conversation when the context fills up");
    s += &role_line(&c, "review", "another model than the main one is preferred", "independent review of the changes");
    s += &role_line(&c, "advisor", "off", "second opinion on a plan before big edits");
    s += &role_line(&c, "challenge", "off", "argues against risky plans before melon acts");
    s
}

/// Writes the file if the repo is a git repo with no `.melon/config.toml` yet. Returns the path when it wrote one.
pub fn write_if_new(cfg: &Config) -> Option<PathBuf> {
    let f = cfg.cwd.join(".melon/config.toml");
    if f.exists() || !cfg.cwd.join(".git").exists() {
        return None;
    }
    std::fs::create_dir_all(f.parent()?).ok()?;
    std::fs::write(&f, text(cfg)).ok()?;
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_role_and_writes_nothing_live() {
        let mut cfg = Config::default();
        cfg.routing.insert("fast".into(), vec!["small-model".into()]);
        let t = text(&cfg);
        assert!(t.contains("# fast      = [\"small-model\"]"));
        assert!(t.contains("explore") && t.contains("not set: scout uses the main model"));
        assert!(t.lines().all(|l| l.is_empty() || l.starts_with('#')), "every line is commented out");
    }
}
