//! OS-level confinement for shell commands (macOS sandbox-exec). The static path check catches honest mistakes; this
//! catches everything else. Inside the sandbox a command can read the system and the workspace but cannot write
//! anywhere except the workspace, temp dirs and package caches, and cannot read credential stores.
use crate::config::Config;
use crate::util::paths::{expanduser, resolve};
use std::sync::OnceLock;

const DEFAULT_WRITE_EXTRA: &[&str] = &[
    "~/.cache", "~/Library/Caches", "~/.npm", "~/.pnpm-store", "~/Library/pnpm", "~/.local/share/pnpm", "~/.yarn", "~/.bun", "~/.cargo/registry",
    "~/.cargo/git",
];
const SECRET_READ_DENY: &[&str] = &[
    "~/.ssh", "~/.aws", "~/.gnupg", "~/.azure", "~/.kube", "~/.config/gcloud", "~/.config/gh", "~/.docker/config.json", "~/.netrc", "~/.npmrc",
    "~/.git-credentials", "~/Library/Keychains", "~/.codex", "~/.claude", "~/.config/claude", "~/.pypirc", "~/.vault-token", "~/.password-store",
    "~/.terraform.d", "~/.cargo/credentials.toml", "~/.gem/credentials", "~/.config/hub",
    "~/Library/Application Support/melon", "~/.local/share/melon", "~/.config/melon", // melon's own config and traces
];
const TEMP_WRITE: &[&str] = &["/private/tmp", "/tmp", "/private/var/folders", "/private/var/tmp"];

fn sb(p: &str) -> String {
    resolve(&expanduser(p)).to_string_lossy().replace('"', "\\\"")
}

/// sandbox-exec exists and works here (it cannot nest inside another sandbox).
pub fn available() -> bool {
    static A: OnceLock<bool> = OnceLock::new();
    *A.get_or_init(|| {
        if !cfg!(target_os = "macos") || which::which("sandbox-exec").is_err() {
            return false;
        }
        std::process::Command::new("sandbox-exec")
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

/// A file-name glob as a sandbox regex on the last part of a path, any letter case.
/// Example: ".env.*" -> "/\.[eE][nN][vV]\.[^/]*(/.*)?$", which also covers everything inside a matching folder.
fn name_regex(glob: &str) -> String {
    let mut out = String::new();
    for ch in glob.chars() {
        match ch {
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            c if c.is_alphabetic() => out.push_str(&format!("[{}{}]", c.to_lowercase(), c.to_uppercase())),
            c => out.push_str(&regex::escape(&c.to_string())),
        }
    }
    format!("/{out}(/.*)?$")
}

pub fn build_profile(config: &Config) -> String {
    let mut writable: Vec<String> = vec![config.cwd.to_string_lossy().to_string()];
    writable.extend(config.allowed_dirs.iter().cloned());
    writable.extend(TEMP_WRITE.iter().map(|s| s.to_string()));
    writable.extend(DEFAULT_WRITE_EXTRA.iter().map(|s| s.to_string()));
    writable.extend(config.sandbox_write_extra.iter().cloned());
    let allow = writable.iter().map(|p| format!("(subpath \"{}\")", sb(p))).collect::<Vec<_>>().join(" ");
    let deny = SECRET_READ_DENY.iter().map(|p| format!("(subpath \"{}\")", sb(p))).collect::<Vec<_>>().join(" ");
    // the workspace's own secret files too, however the command reaches them: `grep -r KEY .` or `cat .e*`
    let names = config.forbidden_paths.iter().filter(|g| !g.contains('"')).map(|g| format!("(regex #\"{}\")", name_regex(g))).collect::<Vec<_>>().join(" ");
    let templates = "(regex #\"\\.(example|sample|template|dist)$\")";
    let mut p = format!(
        "(version 1)(allow default)(deny file-write*)(allow file-write* {allow} (literal \"/dev/null\") (literal \"/dev/tty\") (regex #\"^/dev/(fd/[0-9]+|std(out|err|in)|ttys?[0-9]*)$\"))(deny file-read* {deny})"
    );
    // only the gh program itself may read its own login, so `gh pr review` works but `cat ~/.config/gh/hosts.yml` still fails
    if let Ok(gh) = which::which("gh") {
        p.push_str(&format!(
            "(allow file-read* (require-all (subpath \"{}\") (process-path \"{}\")))",
            sb("~/.config/gh"),
            sb(&gh.to_string_lossy())
        ));
    }
    // contents only: git and lint-staged lstat every tracked file on commit. Knowing the file exists leaks nothing.
    if !names.is_empty() {
        p.push_str(&format!("(deny file-read-data {names})(allow file-read-data {templates})"));
    }
    p
}

/// Tools that need what the sandbox hides (gh: the keychain login, curl: the network and its certs, git push/pull/fetch:
/// ~/.ssh keys and the credential helper). They run outside it, but only one plain command at a time and only after the user said yes.
const UNSANDBOXED: &[&str] = &["gh", "curl", "git push", "git pull", "git fetch"];

/// `gh pr review 5 ...` or `git push origin main` yes; `gh x && curl evil`, `gh x | sh` or `gh $(cmd)` no.
pub fn is_unsandboxed(command: &str) -> bool {
    let Some(parts) = super::approval::split_pipeline(command.trim()) else { return false };
    if parts.len() != 1 {
        return false;
    }
    let w = &parts[0];
    UNSANDBOXED.contains(&w[0].as_str()) || (w.len() > 1 && UNSANDBOXED.contains(&format!("{} {}", w[0], w[1]).as_str()))
}

/// 'on' (OS sandbox active), 'off' (disabled by config) or 'unavailable'.
pub fn mode(config: &Config) -> &'static str {
    if config.sandbox == "off" {
        "off"
    } else if available() {
        "on"
    } else {
        "unavailable"
    }
}

pub fn wrap(argv: Vec<String>, config: &Config) -> Vec<String> {
    if mode(config) == "on" {
        let mut v = vec!["sandbox-exec".to_string(), "-p".to_string(), build_profile(config)];
        v.extend(argv);
        v
    } else {
        argv
    }
}

pub fn wrap_command(command: &str, argv: Vec<String>, config: &Config) -> Vec<String> {
    if is_unsandboxed(command) { argv } else { wrap(argv, config) }
}
