//! Decides whether a tool call may run now, needs the user's yes, or is refused outright.
use super::rules::{load_rules, Rules};
use crate::config::ApprovalPolicy;
use crate::tools::ToolConfirmation;
use crate::util::re::{lit, lit_i, Re};
use futures::future::BoxFuture;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
    NeedsConfirmation,
}

pub struct ApprovalContext {
    pub tool_name: String,
    pub is_mutating: bool,
    pub affected_paths: Vec<PathBuf>,
    pub command: Option<String>,
    pub is_dangerous: bool,
    /// "write" for file edits, "shell", ...
    pub tool_kind: String,
}

/// What the user chose at a prompt. `always` = stop asking for this kind of action for the rest of the session;
/// `feedback` = what to do instead.
#[derive(Clone, Debug, Default)]
pub struct Decision {
    pub approved: bool,
    pub always: bool,
    pub feedback: Option<String>,
}

pub type ConfirmCallback = Arc<dyn Fn(ToolConfirmation) -> BoxFuture<'static, Decision> + Send + Sync>;

const TWO_WORD: &[&str] = &["git", "npm", "pnpm", "yarn", "docker", "cargo", "make", "npx", "uv", "pip", "go"];

/// Commands where the first word alone is too broad to remember ("git status" vs "git push").
pub fn command_key(command: &str) -> String {
    let words: Vec<&str> = command.split_whitespace().collect();
    match words.first() {
        None => String::new(),
        Some(w) if TWO_WORD.contains(w) && words.len() > 1 => words[..2].join(" "),
        Some(w) => w.to_string(),
    }
}

const DANGEROUS_PATTERNS: &[&str] = &[
    r"rm\s+(-rf?|--recursive)\s+[/~]",
    r"rm\s+-rf?\s+\*",
    r"rmdir\s+[/~]",
    r"rm\s+(-\S+\s+)*\S*\.git(/|\s|$)",
    r"git\s+reset\s+--hard",
    r"git\s+clean\s+-\S*f",
    r"git\s+push\s+.*(--force|\s-f(\s|$))",
    r"git\s+(checkout|restore)\s+(--\s+)?\.(\s|$)",
    r"git\s+branch\s+(?-i:-D)", // -d (safe delete) is fine, -D is not
    r"dd\s+if=",
    r"mkfs",
    r"fdisk",
    r"parted",
    r"shutdown",
    r"reboot",
    r"halt",
    r"poweroff",
    r"init\s+[06]",
    r"chmod\s+(-R\s+)?777\s+[/~]",
    r"chown\s+-R\s+.*\s+[/~]",
    r"nc\s+-l",
    r"netcat\s+-l",
    r"curl\s+.*\|\s*(bash|sh)",
    r"wget\s+.*\|\s*(bash|sh)",
    r":\(\)\s*\{\s*:\|:&\s*\}\s*;",
];

fn dangerous() -> &'static Vec<Re> {
    static R: OnceLock<Vec<Re>> = OnceLock::new();
    R.get_or_init(|| DANGEROUS_PATTERNS.iter().map(|p| lit_i(p)).collect())
}

fn plain_safe(name: &str) -> Option<&'static [&'static str]> {
    // `name: reject` lists flags that turn an otherwise read-only command into a writer.
    // env, printenv and set are left out on purpose: they print every secret in the environment.
    Some(match name {
        "ls" | "pwd" | "echo" | "cat" | "head" | "wc" | "which" | "whereis" | "file" | "stat" | "basename" | "dirname" | "realpath" | "grep" | "cut"
        | "uniq" | "tr" | "diff" | "comm" | "uname" | "whoami" | "id" | "groups" | "uptime" | "ps" | "pgrep" | "cd" | "true" | "nl" => &[],
        "tail" => &["-f", "-F", "--follow"],
        "sort" => &["-o", "--output"],
        "date" => &["-s", "--set"],
        "find" => &["-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fprintf", "-fls"],
        "rg" => &["--pre", "--pre-glob"],
        "fd" => &["-x", "--exec", "-X", "--exec-batch"],
        "tree" => &["-o"],
        _ => return None,
    })
}

const GIT_READ: &[&str] = &["status", "log", "diff", "show", "blame", "rev-parse", "ls-files", "shortlog", "describe", "grep"];
const GIT_BAD_FLAGS: &[&str] = &["--output", "--ext-diff", "--textconv", "--open-files-in-pager", "-c", "-C", "--exec-path", "--git-dir", "--work-tree"];

fn git_listing(sub: &str) -> Option<&'static [&'static str]> {
    Some(match sub {
        "branch" => &["-a", "-r", "-v", "-vv", "--list", "--show-current", "--all"],
        "tag" => &["-l", "--list"],
        "remote" => &["-v"],
        _ => return None,
    })
}

fn pkg_read(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "npm" | "pnpm" => &["list", "ls", "outdated"],
        "yarn" => &["list", "outdated"],
        "pip" => &["list", "show", "freeze"],
        "cargo" => &["tree"],
        _ => return None,
    })
}

fn sed_print(a: &str) -> bool {
    // sed only as a line printer: `sed -n '10,40p' a.py`. Its w and e commands write files and run programs.
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"^(\d+|\$)(,(\d+|\$))?p$")).is_match(a)
}

/// The commands of `a | b && c; d` as word lists, or None when the line does more than run commands one after
/// another: a redirect into a file, background (&), substitution, a subshell, an unquoted backslash or a second
/// line. Quotes are respected, so `grep -E "foo|bar" src` is one command, not a pipe. A refusal only means "ask".
pub fn split_pipeline(command: &str) -> Option<Vec<Vec<String>>> {
    if command.contains('`') || command.contains("$(") || command.contains("${") {
        return None;
    }
    let chars: Vec<(usize, char)> = command.char_indices().collect();
    let (mut parts, mut cur): (Vec<String>, String) = (vec![], String::new());
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let (byte, c) = chars[i];
        if let Some(q) = quote {
            if c == '\\' && q == '"' {
                // inside double quotes a backslash only escapes the next character
                cur.push(c);
                if let Some((_, n)) = chars.get(i + 1) {
                    cur.push(*n);
                }
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if let Some(skip) = harmless_redirect(&command[byte..]) {
            // advance by `skip` bytes
            let target = byte + skip;
            while i < chars.len() && chars[i].0 < target {
                i += 1;
            }
            continue;
        } else if let Some(op) = ["&&", "||", "|", ";"].iter().find(|o| command[byte..].starts_with(**o)) {
            parts.push(std::mem::take(&mut cur));
            let target = byte + op.len();
            while i < chars.len() && chars[i].0 < target {
                i += 1;
            }
            continue;
        } else if "<>&()\\\n".contains(c) {
            return None;
        }
        cur.push(c);
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    parts.push(cur);
    let mut out = vec![];
    for part in parts {
        let words = shell_words::split(&part).ok()?;
        if words.is_empty() {
            return None;
        }
        out.push(words);
    }
    Some(out)
}

/// Output thrown away or merged into the pipe writes nothing, so `grep -rn x . 2>/dev/null` stays a read.
/// Returns how many bytes to skip when `rest` starts with one.
fn harmless_redirect(rest: &str) -> Option<usize> {
    static R: OnceLock<[Re; 2]> = OnceLock::new();
    let res = R.get_or_init(|| [lit(r"^[12&]?>\s*/dev/null"), lit(r"^2>&1")]);
    for r in res {
        if let Some((_, e)) = r.find(rest) {
            let next = rest[e..].chars().next();
            if next.map_or(true, |n| n.is_whitespace() || "|;&".contains(n)) {
                return Some(e);
            }
        }
    }
    None
}

/// Catches every spelling of a flag: `-o`, `--output=x` and `-ro` (short flags bundled together).
fn has_flag(args: &[String], flags: &[&str]) -> bool {
    let short: HashSet<char> = flags.iter().filter(|f| f.len() == 2 && !f.ends_with('-')).filter_map(|f| f.chars().nth(1)).collect();
    args.iter().any(|a| {
        flags.contains(&a.as_str())
            || flags.contains(&a.split('=').next().unwrap_or(""))
            || (a.starts_with('-') && !a.starts_with("--") && a.chars().skip(1).any(|c| short.contains(&c)))
    })
}

fn safe_words(words: &[String]) -> bool {
    let (name, args) = (words[0].as_str(), &words[1..]);
    if let Some(reject) = plain_safe(name) {
        return !has_flag(args, reject);
    }
    if name == "sed" {
        return args.len() >= 2 && args[0] == "-n" && sed_print(&args[1]) && !args[2..].iter().any(|a| a.starts_with('-'));
    }
    if name == "git" {
        if args.is_empty() || args.iter().any(|a| GIT_BAD_FLAGS.iter().any(|f| a.starts_with(f))) {
            return false;
        }
        let (sub, rest) = (args[0].as_str(), &args[1..]);
        if GIT_READ.contains(&sub) {
            return true;
        }
        if sub == "stash" {
            return !rest.is_empty() && (rest[0] == "list" || rest[0] == "show");
        }
        return git_listing(sub).map_or(false, |ok| rest.iter().all(|r| ok.contains(&r.as_str())));
    }
    if let Some(ok) = pkg_read(name) {
        return !args.is_empty() && ok.contains(&args[0].as_str());
    }
    false
}

pub fn is_safe_command(command: &str) -> bool {
    match split_pipeline(command.trim()) {
        Some(p) if !p.is_empty() => p.iter().all(|w| safe_words(w)),
        _ => false,
    }
}

pub fn is_dangerous_command(command: &str) -> bool {
    dangerous().iter().any(|r| r.is_match(command))
}

pub struct ApprovalManager {
    pub approval_policy: Mutex<ApprovalPolicy>,
    pub cwd: PathBuf,
    pub confirmation_callback: Mutex<Option<ConfirmCallback>>,
    always_commands: Mutex<HashSet<String>>,
    verify_commands: HashSet<String>,
    rules: Mutex<Rules>,
    /// Total seconds spent waiting for the user's answer; the agent's clock stops meanwhile.
    pub waited: Mutex<f64>,
    unsandboxed: fn(&str) -> bool,
}

impl ApprovalManager {
    pub fn new(policy: ApprovalPolicy, cwd: &Path, verify_commands: &[String]) -> ApprovalManager {
        let cwd = crate::util::paths::resolve(cwd); // symlinked tmp dirs (/var -> /private/var) must still match
        ApprovalManager {
            approval_policy: Mutex::new(policy),
            rules: Mutex::new(load_rules(&cwd)),
            cwd,
            confirmation_callback: Mutex::new(None),
            always_commands: Mutex::new(HashSet::new()),
            verify_commands: verify_commands.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect(),
            waited: Mutex::new(0.0),
            unsandboxed: super::sandbox::is_unsandboxed,
        }
    }

    /// The permission rule that applies to this call, if any.
    pub fn decide_rule(&self, name: &str, params: &serde_json::Value) -> Option<super::rules::Rule> {
        self.rules.lock().unwrap().decide(name, params).cloned()
    }

    pub fn rules_snapshot(&self) -> Rules {
        self.rules.lock().unwrap().clone()
    }

    /// Re-read the allow / deny / ask files (after /permissions changed one).
    pub fn reload_rules(&self) {
        *self.rules.lock().unwrap() = load_rules(&self.cwd);
    }

    pub fn policy(&self) -> ApprovalPolicy {
        *self.approval_policy.lock().unwrap()
    }

    pub fn set_policy(&self, p: ApprovalPolicy) {
        *self.approval_policy.lock().unwrap() = p;
    }

    pub fn has_callback(&self) -> bool {
        self.confirmation_callback.lock().unwrap().is_some()
    }

    pub fn waited(&self) -> f64 {
        *self.waited.lock().unwrap()
    }

    /// The repo's own check, e.g. `pnpm check-types`, optionally followed by `2>&1` and a safe pipe like `| tail -20`.
    /// ponytail: exact match only; `cd dir && pnpm test` still asks.
    fn is_verify_command(&self, command: &str) -> bool {
        let (first, rest) = match command.trim().split_once('|') {
            Some((a, b)) => (a, Some(b)),
            None => (command.trim(), None),
        };
        let first = first.trim();
        let first = first.strip_suffix("2>&1").unwrap_or(first).trim();
        if !self.verify_commands.contains(first) {
            return false;
        }
        rest.map_or(true, is_safe_command)
    }

    /// A remembered "don't ask again" covers the command the user saw, not whatever is chained after it.
    fn always_allowed(&self, command: &str) -> bool {
        let always = self.always_commands.lock().unwrap();
        match split_pipeline(command.trim()) {
            Some(p) if !p.is_empty() => p.iter().all(|w| always.contains(&command_key(&w.join(" ")))),
            _ => false,
        }
    }

    fn assess_command_safety(&self, command: &str) -> ApprovalDecision {
        use ApprovalDecision::*;
        use ApprovalPolicy as P;
        let policy = self.policy();
        if policy == P::Yolo {
            return Approved;
        }
        if policy == P::Plan {
            return if is_safe_command(command) { Approved } else { Rejected };
        }
        if is_dangerous_command(command) {
            return Rejected;
        }
        if self.always_allowed(command) || self.is_verify_command(command) {
            return Approved;
        }
        if (self.unsandboxed)(command) {
            // runs outside the sandbox, so it is the user's call every time, even in auto mode
            return if policy == P::Never { Rejected } else { NeedsConfirmation };
        }
        match policy {
            P::Never => {
                if is_safe_command(command) { Approved } else { Rejected }
            }
            P::Auto | P::OnFailure => Approved,
            P::AutoEdit => {
                if is_safe_command(command) { Approved } else { NeedsConfirmation }
            }
            _ => {
                if is_safe_command(command) { Approved } else { NeedsConfirmation }
            }
        }
    }

    pub fn check_approval(&self, ctx: &ApprovalContext) -> ApprovalDecision {
        use ApprovalDecision::*;
        use ApprovalPolicy as P;
        if !ctx.is_mutating {
            return Approved;
        }
        if let Some(c) = &ctx.command {
            // a command that needs confirmation must stay that way
            return self.assess_command_safety(c);
        }
        let policy = self.policy();
        if policy == P::Yolo {
            return Approved;
        }
        if policy == P::Plan {
            return Rejected;
        }
        let outside = ctx.affected_paths.iter().any(|p| !crate::util::paths::resolve(p).starts_with(&self.cwd));
        if outside {
            return if policy == P::Never { Rejected } else { NeedsConfirmation };
        }
        if ctx.tool_kind == "write" {
            // file edits: ask by default; "auto-edit"/"auto" pre-approve edits inside the workspace
            return match policy {
                P::Auto | P::AutoEdit | P::OnFailure => Approved,
                P::Never => Rejected,
                _ => NeedsConfirmation,
            };
        }
        if ctx.is_dangerous && !matches!(policy, P::Auto | P::AutoEdit) {
            return NeedsConfirmation;
        }
        Approved
    }

    /// Ask the user. With nobody to ask (single-run mode), fail closed instead of approving silently.
    pub async fn request_confirmation(&self, confirmation: ToolConfirmation) -> Decision {
        let cb = self.confirmation_callback.lock().unwrap().clone();
        let Some(cb) = cb else {
            return Decision {
                approved: false,
                always: false,
                feedback: Some(
                    "This needs approval but the run is non-interactive. Re-run with --approval auto-edit (allow edits) or --approval auto (edits and commands)."
                        .into(),
                ),
            };
        };
        let started = Instant::now();
        let d = cb(confirmation).await;
        *self.waited.lock().unwrap() += started.elapsed().as_secs_f64();
        d
    }

    /// Apply a 'don't ask again' choice for the rest of the session.
    pub fn remember(&self, confirmation: &ToolConfirmation) {
        if let Some(c) = &confirmation.command {
            self.always_commands.lock().unwrap().insert(command_key(c));
        } else {
            self.set_policy(ApprovalPolicy::AutoEdit); // edits only; commands still ask
        }
    }
}

#[allow(dead_code)]
fn _unused(_: HashMap<u8, u8>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_and_unsafe() {
        assert!(is_safe_command("ls -la | head"));
        assert!(is_safe_command("git status && git diff"));
        assert!(is_safe_command("grep -rn x . 2>/dev/null"));
        assert!(!is_safe_command("ls; rm x"));
        assert!(!is_safe_command("cat a > b"));
        assert!(!is_safe_command("ls $(rm x)"));
        assert!(!is_safe_command("find . -delete"));
        assert!(!is_safe_command("git push"));
        assert!(is_safe_command("sed -n '10,40p' a.py"));
        assert!(!is_safe_command("sed -i s/a/b/ f"));
    }

    #[test]
    fn dangerous() {
        assert!(is_dangerous_command("rm -rf /"));
        assert!(is_dangerous_command("git reset --hard HEAD"));
        assert!(!is_dangerous_command("git branch -d x"));
        assert!(is_dangerous_command("git branch -D x"));
    }
}
