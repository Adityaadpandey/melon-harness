//! First-run setup (`zerd setup`) and the health check (`zerd doctor`).
//!
//! Setup runs by itself the first time you start zerd with no usable model. It asks where your models come from,
//! saves the key, proves it works by calling the provider, picks a model, and asks how much zerd should ask before
//! acting. Doctor answers "why isn't this working?" with one line per thing that can go wrong, each with its fix.
use crate::client::providers::{self, Provider};
use crate::config::loader::{get_system_config_path, get_data_dir, save_env_key, save_last_model};
use crate::config::{ApprovalPolicy, Config};
use crate::ui::style::*;
use std::io::{BufRead, Write};

/// Read one line from stdin; None on Ctrl-D so every question can treat "the user left" as a skip.
fn ask(prompt: &str) -> Option<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    match std::io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s.trim().to_string()),
    }
}

/// A numbered menu. Returns the 0-based pick, or None for skip/Ctrl-D. Enter picks `default`.
fn menu(title: &str, items: &[String], default: usize) -> Option<usize> {
    println!("\n{}", bold_paint(title, LAVENDER));
    for (i, it) in items.iter().enumerate() {
        println!("  {} {it}", bold_paint(&format!("{}{}", i + 1, if i == default { ")" } else { ")" }), ACCENT));
    }
    loop {
        let a = ask(&paint(&format!("Pick 1-{} (Enter = {}, s = skip): ", items.len(), default + 1), MUTED))?;
        match a.as_str() {
            "" => return Some(default),
            "s" | "q" => return None,
            n => match n.parse::<usize>() {
                Ok(k) if (1..=items.len()).contains(&k) => return Some(k - 1),
                _ => println!("{}", paint("  that isn't one of the numbers above", AMBER)),
            },
        }
    }
}

/// Models worth showing first: the families people actually mean, then the rest, newest-sounding first.
/// Example: for anthropic, "claude-sonnet-5-5" floats above "claude-3-haiku".
fn rank_models(mut ids: Vec<String>) -> Vec<String> {
    const SKIP: &[&str] = &["embed", "whisper", "tts", "dall-e", "moderation", "transcribe", "image", "audio", "realtime", "guard"];
    const LIKE: &[&str] = &["sonnet", "gpt-5", "gemini-3", "gemini-2.5-pro", "opus"];
    ids.retain(|i| !SKIP.iter().any(|s| i.to_lowercase().contains(s)));
    ids.sort_by(|a, b| b.cmp(a));
    ids.sort_by_key(|i| !LIKE.iter().any(|l| i.to_lowercase().contains(l)));
    ids
}

/// Set a top-level `key = value` line in a TOML file's text: replace it if present, else put it first.
/// Putting it first matters: after a `[table]` header the line would belong to that table.
fn set_top_level(text: &str, key: &str, value: &str) -> String {
    let mut out = vec![format!("{key} = \"{value}\"")];
    let mut in_table = false;
    for line in text.lines() {
        in_table |= line.trim_start().starts_with('[');
        if !in_table && line.trim_start().starts_with(&format!("{key} ")) || !in_table && line.trim_start().starts_with(&format!("{key}=")) {
            continue;
        }
        out.push(line.to_string());
    }
    out.join("\n") + "\n"
}

fn save_approval(value: &str) -> std::io::Result<()> {
    let path = get_system_config_path();
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(path, set_top_level(&old, "approval", value))
}

/// The provider a model id talks to, for connection checks. The Command Code gateway has no preset, so build it here.
fn provider_for(cfg: &Config, model: &str) -> Option<Provider> {
    if let Some((p, _)) = providers::split(cfg, model) {
        return Some(p);
    }
    if model.starts_with("codex/") {
        return None;
    }
    Some(Provider { name: "gateway".into(), base_url: cfg.base_url(), api_key_env: Some("API_KEY".into()), protocol: "openai".into(), headers: Default::default(), compat: Default::default() })
}

fn codex_default_model() -> String {
    let cache = crate::client::codex_auth::auth_path().parent().map(|p| p.join("models_cache.json"));
    cache
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["models"].as_array().and_then(|a| a.iter().find(|m| m["visibility"] == "list").and_then(|m| m["slug"].as_str().map(String::from))))
        .map(|s| format!("codex/{s}"))
        .unwrap_or_else(|| "codex/gpt-5.6-luna".into())
}

/// Ask for a provider's key until it works (three tries). Returns the model list the provider gave back.
async fn connect_provider(p: &Provider) -> Option<Vec<String>> {
    for attempt in 0..3 {
        if let Some(env) = &p.api_key_env {
            let have = p.api_key().is_some();
            let note = if have { " (Enter keeps the one already saved)" } else { "" };
            let key = crate::ui::app::read_secret_line(&format!("Paste your {} key{note}: ", p.name));
            println!();
            if key.chars().any(char::is_whitespace) {
                println!("{}", paint("  that has spaces or line breaks, so it isn't a key. Try again.", AMBER));
                continue;
            }
            if key.is_empty() && !have {
                return None;
            }
            if !key.is_empty() {
                if let Err(e) = save_env_key(env, &key) {
                    println!("{}", paint(&format!("  couldn't save the key: {e}"), CORAL));
                    return None;
                }
            }
        }
        print!("{}", paint("  checking with the provider... ", MUTED));
        let _ = std::io::stdout().flush();
        match providers::fetch_models(p).await {
            Ok(ids) => {
                println!("{}", paint("works", MINT));
                return Some(ids);
            }
            Err(e) => {
                let hint = if p.api_key_env.is_none() { format!("is {} running? start it, then run `zerd setup` again", p.name) } else if e.contains("401") || e.contains("403") { "the key was rejected, check you copied all of it".into() } else { "check your connection".to_string() };
                println!("{}", paint(&format!("failed ({}). {hint}", e.chars().take(80).collect::<String>()), CORAL));
                if p.api_key_env.is_none() || attempt == 2 {
                    return None;
                }
            }
        }
    }
    None
}

/// True when zerd can now call a model. `first_run` only changes the greeting.
pub async fn setup(cfg: &Config, first_run: bool) -> bool {
    run_setup(cfg, first_run).await.unwrap_or(false)
}

async fn run_setup(cfg: &Config, first_run: bool) -> Option<bool> {
    if first_run {
        println!("\n{} {}", bold_paint("✦ Welcome to zerd.", ACCENT), paint("Let's get you to a first task in about a minute.", MUTED));
    }
    let codex = crate::client::codex_auth::auth_path().is_file();
    let items: Vec<String> = vec![
        format!("ChatGPT / Codex subscription      {}", if codex { paint("login found, no key needed", MINT) } else { paint("needs `codex login` first", MUTED) }),
        "Anthropic API key (Claude)".into(),
        "OpenAI API key".into(),
        "OpenRouter (many models, one key)".into(),
        "Google Gemini API key".into(),
        "Local model (Ollama or LM Studio, no key)".into(),
        "Command Code gateway key".into(),
        "Another provider (groq, deepseek, mistral, xai, ...)".into(),
    ];
    let Some(pick) = menu("Where should the models come from?", &items, if codex { 0 } else { 1 }) else {
        println!("{}", paint("\nSkipped. Run `zerd setup` whenever you want to come back, or /connect inside zerd.", MUTED));
        return Some(false);
    };
    let model: Option<String> = match pick {
        0 => {
            if !codex {
                println!("{}", paint("\nInstall the Codex CLI, run `codex login`, then `zerd setup` again. zerd reuses that login.", AMBER));
                return Some(false);
            }
            match crate::client::codex_auth::CodexAuth::default().credentials(false).await {
                Ok(_) => println!("{}", paint("  ChatGPT login works", MINT)),
                Err(e) => {
                    println!("{}", paint(&format!("  {e}"), CORAL));
                    return Some(false);
                }
            }
            Some(codex_default_model())
        }
        6 => {
            let p = provider_for(cfg, "gateway-model").expect("gateway provider");
            connect_provider(&p).await?;
            None // the gateway keeps its configured default model
        }
        n => {
            let name = match n {
                1 => "anthropic".to_string(),
                2 => "openai".into(),
                3 => "openrouter".into(),
                4 => "gemini".into(),
                5 => {
                    let local = ["ollama", "lmstudio"];
                    let i = menu("Which one?", &local.map(String::from), 0)?;
                    local[i].to_string()
                }
                _ => {
                    let all = providers::all_providers(cfg);
                    let names: Vec<&String> = all.keys().filter(|k| !["anthropic", "openai", "openrouter", "gemini", "ollama", "lmstudio"].contains(&k.as_str())).collect();
                    let a = ask(&paint(&format!("\nWhich one? ({}): ", names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")), MUTED))?;
                    if !all.contains_key(&a) {
                        println!("{}", paint(&format!("  unknown provider {a:?}; add a [providers.{a}] table to config.toml for a custom one"), AMBER));
                        return Some(false);
                    }
                    a
                }
            };
            let p = providers::all_providers(cfg).remove(&name)?;
            let ids = rank_models(connect_provider(&p).await?);
            if ids.is_empty() {
                println!("{}", paint(&format!("  {name} listed no models. Pick one by name later: /model {name}:<model>"), AMBER));
                return Some(true);
            }
            let shown: Vec<String> = ids.iter().take(9).cloned().collect();
            let i = menu("Which model should be the default?", &shown, 0).unwrap_or(0);
            Some(format!("{name}:{}", shown[i]))
        }
    };
    if let Some(m) = &model {
        save_last_model(m);
        println!("{}", paint(&format!("  default model: {m} (change it any time with /model)"), MUTED));
    }
    let levels: Vec<String> = vec![
        "Ask before edits and commands          (on-request, the safe start)".into(),
        "Edit files freely, ask for commands    (auto-edit)".into(),
        "Run everything, workspace-confined     (auto)".into(),
    ];
    let values = ["on-request", "auto-edit", "auto"];
    if let Some(i) = menu("How much should zerd ask before acting?", &levels, 0) {
        if ApprovalPolicy::parse(values[i]).is_some() && save_approval(values[i]).is_ok() {
            println!("{}", paint(&format!("  approval: {} (change with /approval, saved in {})", values[i], get_system_config_path().display()), MUTED));
        }
    }
    println!(
        "\n{}\n  {}  {}\n  {}  {}\n  {}",
        bold_paint("✓ You're set.", MINT),
        bold_paint("cd your-repo && zerd", ACCENT),
        paint("then ask for something small, like \"find why the tests are slow\"", MUTED),
        bold_paint("zerd \"fix the failing test\"", ACCENT),
        paint("runs one task and prints what was verified", MUTED),
        paint("Something off? `zerd doctor` checks everything and says how to fix it.\n", MUTED),
    );
    Some(true)
}

enum Level {
    Ok,
    Warn,
    Bad,
}

fn line(level: Level, what: &str, detail: &str, fix: &str) -> bool {
    let (mark, color) = match level {
        Level::Ok => ("✓", MINT),
        Level::Warn => ("!", AMBER),
        Level::Bad => ("✗", CORAL),
    };
    let fix = if fix.is_empty() { String::new() } else { format!("\n      {}", paint(&format!("fix: {fix}"), MUTED)) };
    println!("  {} {:<14}{}{fix}", bold_paint(mark, color), what, detail);
    !matches!(level, Level::Bad)
}

/// Print one line per check. Returns the exit code: 1 if something is broken, 0 otherwise.
pub async fn doctor(cfg: &Config) -> i32 {
    println!("\n{}", bold_paint(&format!("zerd {} · doctor", env!("CARGO_PKG_VERSION")), LAVENDER));
    let mut ok = true;
    let model = cfg.model_name().to_string();

    ok &= match cfg.missing_key(None) {
        None => line(Level::Ok, "model", &format!("{model} has credentials"), ""),
        Some(why) => line(Level::Bad, "model", &why, "run `zerd setup`"),
    };
    if cfg.missing_key(None).is_none() {
        if let Some(p) = provider_for(cfg, &model) {
            ok &= match providers::fetch_models(&p).await {
                Ok(ids) => line(Level::Ok, "connection", &format!("{} answered, {} models", p.name, ids.len()), ""),
                Err(e) => line(Level::Bad, "connection", &format!("{} failed: {}", p.name, e.chars().take(80).collect::<String>()), "check the key and your network, or run `zerd setup`"),
            };
        } else {
            ok &= match crate::client::codex_auth::CodexAuth::default().credentials(false).await {
                Ok(_) => line(Level::Ok, "connection", "ChatGPT login is valid", ""),
                Err(e) => line(Level::Bad, "connection", &e, "run `codex login`"),
            };
        }
    }
    ok &= if which::which("git").is_ok() { line(Level::Ok, "git", "found", "") } else { line(Level::Bad, "git", "not installed, zerd needs it for diffs, checkpoints and reviews", "install git (Windows: Git for Windows)") };
    let in_repo = std::process::Command::new("git").args(["rev-parse", "--git-dir"]).current_dir(&cfg.cwd).output().map(|o| o.status.success()).unwrap_or(false);
    if in_repo {
        line(Level::Ok, "repo", &format!("{} is a git repo", cfg.cwd.display()), "");
    } else {
        line(Level::Warn, "repo", "this folder isn't a git repo, so checkpoints and diff review are limited", "git init");
    }
    match crate::safety::sandbox::mode(cfg) {
        "on" => line(Level::Ok, "sandbox", "commands run inside the OS sandbox", ""),
        "off" => line(Level::Warn, "sandbox", "turned off in config", "remove sandbox = \"off\" from config.toml"),
        _ => line(Level::Warn, "sandbox", "not available here (macOS only), approvals and workspace confinement still apply", ""),
    };
    line(Level::Ok, "approval", cfg.approval.value(), "");
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let width = crossterm::terminal::size().map(|s| s.0).unwrap_or(0);
    if !tty {
        line(Level::Warn, "terminal", "output isn't a terminal, the interactive UI won't start", "");
    } else if width < 80 {
        line(Level::Warn, "terminal", &format!("{width} columns is narrow, the side panel needs 80+"), "widen the window");
    } else {
        line(Level::Ok, "terminal", &format!("{width} columns"), "");
    }
    let data = get_data_dir();
    let writable = std::fs::create_dir_all(&data).is_ok() && std::fs::write(data.join(".doctor"), "").is_ok();
    let _ = std::fs::remove_file(data.join(".doctor"));
    ok &= if writable { line(Level::Ok, "data folder", &data.display().to_string(), "") } else { line(Level::Bad, "data folder", &format!("can't write to {}", data.display()), "fix permissions or set ZERD_DATA_DIR") };
    if which::which("zerd").is_err() {
        line(Level::Warn, "PATH", "`zerd` isn't on your PATH from this shell", "add the install folder (~/.local/bin) to PATH");
    }
    println!("\n{}\n", if ok { paint("Nothing blocking. You're good to go.", MINT) } else { paint("Fix the ✗ lines above, then run `zerd doctor` again.", CORAL) });
    if ok { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_line_goes_above_any_table_and_replaces_the_old_one() {
        let out = set_top_level("approval = \"auto\"\n[providers.x]\napproval = \"keep\"\n", "approval", "on-request");
        assert_eq!(out, "approval = \"on-request\"\n[providers.x]\napproval = \"keep\"\n");
        assert_eq!(set_top_level("", "approval", "auto"), "approval = \"auto\"\n");
    }

    #[test]
    fn ranking_drops_non_chat_models_and_floats_the_usual_families() {
        let r = rank_models(vec!["text-embedding-3".into(), "claude-3-haiku".into(), "claude-sonnet-5-5".into()]);
        assert_eq!(r, vec!["claude-sonnet-5-5", "claude-3-haiku"]);
    }
}
