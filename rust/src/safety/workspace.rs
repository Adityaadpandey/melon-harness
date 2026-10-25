//! Workspace confinement. The session lives in the folder it was started in; file tools may not leave it
//! (symlinks included), and shell commands may not name paths in the user's home or other private areas outside it.
use crate::util::paths::{expanduser, normalize, resolve};
use crate::util::re::Re;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Outside the workspace these are private: other projects, dotfiles, credentials.
const SENSITIVE_ROOTS: &[&str] = &["/etc", "/private/etc", "/root", "/var/root", "/Users", "/home", "/Volumes", "/mnt", "/media"];
const TEMP_DIRS: &[&str] = &["/tmp", "/private/tmp", "/var/folders", "/private/var/folders", "/var/tmp", "/private/var/tmp"];

fn cd_re() -> &'static Re {
    // `cd` and `pushd` with their target, which may be missing (plain `cd` goes home)
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| {
        Re::new(r"(?:^|[;&|(]|\b(?:then|do))\s*(?:cd|pushd)(?:\s+([^\s;&|)]+))?\s*(?=$|[;&|)])", false).unwrap()
    })
}

fn path_res() -> &'static [(Re, &'static str)] {
    static R: OnceLock<Vec<(Re, &'static str)>> = OnceLock::new();
    R.get_or_init(|| {
        vec![
            // "~line 97" in prose is not home
            (Re::new(r#"(?<![\w$])~(?![\w-])(?:/[^\s"';|&<>()`]*)?"#, false).unwrap(), "home"),
            (Re::new(r#"\$\{?HOME\}?(?:/[^\s"';|&<>()`]*)?"#, false).unwrap(), "envhome"),
            (Re::new(r#"(?<![\w.:$/\-])/[^\s"';|&<>()`=,]+"#, false).unwrap(), "abs"),
            (Re::new(r#"(?<![\w/.\-])\.\.(?:/[^\s"';|&<>()`]*)?(?![\w.])"#, false).unwrap(), "rel"),
        ]
    })
}

fn git_message() -> &'static Re {
    // the quoted message of `git commit -m "..."`: prose, not paths
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| Re::new(r#"(\bgit\b[^;&|]*?\s(?:-m|--message)(?:\s+|=))("(?:[^"\\]|\\.)*"|'[^']*')"#, false).unwrap())
}

fn real(p: &Path) -> PathBuf {
    resolve(&expanduser(&p.to_string_lossy()))
}

pub struct Workspace {
    pub root: PathBuf,
    extra: Vec<PathBuf>,
    home: PathBuf,
    sensitive: Vec<PathBuf>,
}

impl Workspace {
    pub fn new(root: &Path, extra: &[String]) -> Workspace {
        Workspace {
            root: real(root),
            extra: extra.iter().map(|e| real(Path::new(e))).collect(),
            home: real(&dirs::home_dir().unwrap_or_default()),
            sensitive: SENSITIVE_ROOTS.iter().map(|r| real(Path::new(r))).collect(),
        }
    }

    pub fn roots(&self) -> Vec<&PathBuf> {
        std::iter::once(&self.root).chain(self.extra.iter()).collect()
    }

    fn inside(&self, p: &Path) -> bool {
        self.roots().iter().any(|r| p == r.as_path() || p.starts_with(r))
    }

    /// True if `path` (relative to `base`, default the root) stays inside the workspace after resolving symlinks and `..`.
    pub fn contains(&self, path: &str, base: Option<&Path>) -> bool {
        let mut p = expanduser(path);
        if !p.is_absolute() {
            p = base.unwrap_or(&self.root).join(p);
        }
        self.inside(&real(&p))
    }

    /// First path in `command` that points somewhere private outside the workspace. Static analysis only: the OS
    /// sandbox is the backstop for anything built dynamically ($(...), eval, encoded paths).
    pub fn shell_violation(&self, command: &str, cwd: Option<&Path>) -> Option<String> {
        let base = real(cwd.unwrap_or(&self.root));
        let command = git_message().replace_all(command, "$1''");
        let temps: Vec<PathBuf> = TEMP_DIRS.iter().map(|t| real(Path::new(t))).collect();
        let mut from = 0;
        while let Some(caps) = cd_re().captures(&command[from..]) {
            let whole_end = cd_re().find(&command[from..]).map(|m| m.1).unwrap_or(command.len() - from);
            let to_raw = caps.get(1).cloned().flatten().unwrap_or_else(|| "~".into());
            let to = to_raw.trim_matches(|c| c == '\'' || c == '"').to_string();
            // after `cd /` every relative path means something else, so a shell may only move inside the workspace
            // or into a temp dir. Plain `cd` and `cd -` count as leaving.
            let target = if to != "-" {
                let ex = to.replace("${HOME}", "~").replace("$HOME", "~");
                real(&base.join(expanduser(&ex)))
            } else {
                self.home.clone()
            };
            let inside = self.inside(&target);
            let temp = temps.iter().any(|t| target.starts_with(t)) && !(self.home.starts_with(&target) || self.root.starts_with(&target));
            if !(inside || temp) {
                return Some(format!("'cd {to}' leaves the workspace {}; run the command from inside it", self.root.display()));
            }
            from += whole_end.max(1);
            if from >= command.len() {
                break;
            }
        }
        let home = self.home.to_string_lossy().to_string();
        for (re, _) in path_res() {
            let mut at = 0;
            while at <= command.len() {
                let Some((s, e)) = re.find(&command[at..]) else { break };
                let raw = &command[at + s..at + e];
                at += e.max(1);
                let expanded = raw.replace("${HOME}", &home).replace("$HOME", &home);
                let mut cand = expanduser(&expanded);
                if !cand.is_absolute() {
                    cand = base.join(cand);
                }
                let r = real(&normalize(&cand));
                if self.inside(&r) {
                    continue;
                }
                let private = r == self.home || r.starts_with(&self.home) || self.sensitive.iter().any(|s| r == *s || r.starts_with(s));
                if private {
                    return Some(format!("'{raw}' resolves to {}, outside the workspace {}", r.display(), self.root.display()));
                }
            }
        }
        None
    }
}
