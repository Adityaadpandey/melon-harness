//! Ported from tests/test_workspace.py: the workspace boundary (file tools, shell commands, secrets), the OS sandbox
//! and the network guard.
//!
//! Python swaps in a fake home with monkeypatch. Here HOME is an env var shared by the whole test process, so every
//! test in this file takes one lock, points HOME where it needs it, and puts the real value back when it is done.
//!
//! Not ported: test_fetch_refuses_redirect_to_localhost. It swaps httpx for a mock transport and pretends a made-up
//! host is public; the Rust web_fetch builds its own reqwest client and resolves the real host, so there is no seam
//! to fake a public server that redirects to 127.0.0.1 without real network access.
use crate::config::{ApprovalPolicy, Config};
use crate::safety::guard::violation;
use crate::safety::sandbox;
use crate::safety::workspace::Workspace;
use crate::util::netguard::check_url;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

static LOCK: Mutex<()> = Mutex::new(());
static REAL_HOME: OnceLock<String> = OnceLock::new();

/// Holds the HOME lock; the real HOME comes back on drop.
struct Env {
    _guard: MutexGuard<'static, ()>,
    _dir: tempfile::TempDir,
    /// A project living under a (fake) home dir, like ~/Devlopment/x.
    ws: PathBuf,
    home: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        unsafe { std::env::set_var("HOME", REAL_HOME.get().unwrap()) };
    }
}

fn lock() -> MutexGuard<'static, ()> {
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    REAL_HOME.get_or_init(|| std::env::var("HOME").unwrap_or_default());
    unsafe { std::env::set_var("HOME", REAL_HOME.get().unwrap()) };
    g
}

/// HOME = <tmp>/home, workspace = <tmp>/home/proj with a src/ folder.
fn fake_home() -> Env {
    let guard = lock();
    let dir = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(dir.path()).unwrap().join("home");
    let ws = home.join("proj");
    fs::create_dir_all(ws.join("src")).unwrap();
    unsafe { std::env::set_var("HOME", &home) };
    Env { _guard: guard, _dir: dir, ws, home }
}

fn cfg(ws: &Path) -> Config {
    let mut c = Config::default();
    c.cwd = ws.to_path_buf();
    c
}

fn sandboxed(script: &str, c: &Config, cwd: &Path) -> std::process::Output {
    let argv = sandbox::wrap(vec!["/bin/bash".into(), "-c".into(), script.into()], c);
    Command::new(&argv[0]).args(&argv[1..]).current_dir(cwd).output().unwrap()
}

#[cfg(unix)]
#[test]
fn paths_inside_are_fine_outside_and_symlink_escapes_are_not() {
    let e = fake_home();
    let w = Workspace::new(&e.ws, &[]);
    let inside = |w: &Workspace, p: &str| w.contains(p, None);
    assert!(inside(&w, "src/a.py") && inside(&w, &e.ws.join("x").to_string_lossy()) && inside(&w, "."));
    assert!(!inside(&w, "../other") && !inside(&w, "/etc/passwd") && !inside(&w, "~/.ssh/id_rsa"));
    let secret_dir = e.home.join("private");
    fs::create_dir(&secret_dir).unwrap();
    // a link inside the workspace pointing out of it
    std::os::unix::fs::symlink(&secret_dir, e.ws.join("link")).unwrap();
    assert!(!inside(&w, "link/file.txt"));
    // unless explicitly added
    assert!(inside(&Workspace::new(&e.ws, &[secret_dir.to_string_lossy().to_string()]), "link/file.txt"));
}

#[test]
fn shell_commands_cannot_name_private_paths_outside() {
    let e = fake_home();
    let cmds = [
        "cat ~/.ssh/id_rsa", "cat $HOME/.aws/credentials", "ls ${HOME}/Documents", "cd .. && ls", "cat ../secrets.txt",
        "cp src/a.py /Users/someone/else.py", "python -c \"open('/etc/passwd').read()\"", "ls ~", "cat /home/bob/.bashrc",
    ];
    for cmd in cmds {
        assert!(Workspace::new(&e.ws, &[]).shell_violation(cmd, None).is_some(), "{cmd}");
    }
}

#[test]
fn ordinary_commands_are_not_flagged() {
    let e = fake_home();
    let cmds = [
        "ls src && cat src/a.py", "pnpm test", "cat /dev/null", "ls /usr/bin | head", "echo hi > /tmp/x.txt",
        "curl https://example.com/a/b", "git log --oneline", "cat ./src/../src/a.py", "node -e \"console.log(1)\"",
    ];
    for cmd in cmds {
        assert_eq!(Workspace::new(&e.ws, &[]).shell_violation(cmd, None), None, "{cmd}");
    }
}

#[test]
fn tool_paths_are_confined_through_the_guard() {
    let e = fake_home();
    let c = cfg(&e.ws);
    assert!(violation("read_file", &json!({"path": "src/a.py"}), &c).is_none());
    let why = |tool: &str, p: serde_json::Value, c: &Config| violation(tool, &p, c).unwrap_or_default();
    assert!(why("read_file", json!({"path": "../../etc/passwd"}), &c).contains("outside the workspace"));
    assert!(why("shell", json!({"command": "ls", "cwd": "/"}), &c).contains("outside the workspace"));
    assert!(why("write_file", json!({"path": "/Users/x/evil.sh", "content": ""}), &c).contains("outside the workspace"));
    // secrets blocked by default
    assert!(why("read_file", json!({"path": ".env.production"}), &c).contains("protected pattern"));
    assert!(violation("read_file", &json!({"path": ".env.example"}), &c).is_none());
    assert!(why("some_mcp_tool", json!({"file": "/Users/x/.ssh/id_rsa"}), &c).contains("outside the workspace"));
    let mut allowed = cfg(&e.ws);
    allowed.allowed_dirs = vec!["/x".into()];
    assert!(violation("read_file", &json!({"path": "/x/y"}), &allowed).is_none());
}

#[tokio::test]
async fn registry_blocks_escape_attempts_end_to_end() {
    let e = fake_home();
    let outside = e.home.join("outside.txt");
    fs::write(&outside, "top secret").unwrap();
    let mut c = crate::tests::fakes::config(&e.ws);
    c.approval = ApprovalPolicy::Yolo;
    let mut a = crate::agent::agent::Agent::new(c, None).await;
    let s = a.session.clone();
    let r = s.tool_registry.invoke("read_file", json!({"path": outside.to_string_lossy()}), &e.ws, &s.hook_system, None, "c1").await;
    assert!(!r.success && r.error.clone().unwrap().contains("outside the workspace") && !r.output.contains("top secret"), "{r:?}");
    let r = s.tool_registry.invoke("write_file", json!({"path": "../hacked.txt", "content": "x"}), &e.ws, &s.hook_system, Some(&s.approval_manager), "c2").await;
    assert!(!r.success && !e.home.join("hacked.txt").exists());
    a.close().await;
}

#[cfg(target_os = "macos")]
#[test]
fn os_sandbox_blocks_writes_outside_and_secret_reads_even_when_text_checks_are_bypassed() {
    if !sandbox::available() {
        return; // needs macOS sandbox-exec
    }
    let _g = lock(); // HOME is the real one: the sandbox profile must be built from the real home
    let d = tempfile::tempdir().unwrap();
    let ws = fs::canonicalize(d.path()).unwrap();
    let c = cfg(&ws);
    assert!(sandboxed("echo ok > inside.txt", &c, &ws).status.success() && ws.join("inside.txt").exists());
    let user = std::env::var("USER").unwrap_or_default();
    let real_home = PathBuf::from(std::env::var("HOME").unwrap());
    let victim = real_home.join(format!("melon_sandbox_probe_{}.txt", std::process::id()));
    // built dynamically so no static check could see it; the real home is never writable from the sandbox
    let r = sandboxed(&format!("h=$(eval echo ~{user}); echo x > \"$h/melon_sandbox_probe_{}.txt\"", std::process::id()), &c, &ws);
    assert!(!r.status.success() && !victim.exists());
    let _ = fs::remove_file(&victim);
    assert!(!sandboxed(&format!("ls {}/.ssh", real_home.display()), &c, &ws).status.success());
    assert!(sandboxed("echo t > /tmp/melon_sandbox_probe && rm /tmp/melon_sandbox_probe", &c, &ws).status.success());
    assert!(sandboxed("python3 -c 'print(1)' && git --version", &c, &ws).status.success());
}

#[test]
fn sandbox_off_and_unavailable_modes() {
    let e = fake_home();
    let mut off = cfg(&e.ws);
    off.sandbox = "off".into();
    assert_eq!(sandbox::wrap(vec!["/bin/true".into()], &off), vec!["/bin/true"]);
    assert_eq!(sandbox::mode(&off), "off");
    // Python also monkeypatches available() to False; here that case is whatever this machine really is
    let mode = sandbox::mode(&cfg(&e.ws));
    assert_eq!(mode, if sandbox::available() { "on" } else { "unavailable" });
}

#[test]
fn network_guard_blocks_local_and_private_targets() {
    assert!(check_url("http://localhost:3000/admin", false).is_some());
    assert!(check_url("http://127.0.0.1/", false).is_some());
    assert!(check_url("http://169.254.169.254/latest/meta-data/", false).is_some());
    assert!(check_url("http://192.168.1.10/", false).is_some());
    assert!(check_url("file:///etc/passwd", false).is_some());
    assert!(check_url("http://127.0.0.1/", true).is_none());
    // lets the request fail normally
    assert!(check_url("http://this-host-does-not-exist.invalid/", false).is_none());
}

#[test]
fn a_shell_may_not_cd_out_of_the_workspace() {
    let e = fake_home();
    for cmd in ["cd /; cat etc/passwd", "cd && cat .ssh/config", "cd ~ ; ls", "cd - ; ls", "if true; then cd /; fi"] {
        let why = Workspace::new(&e.ws, &[]).shell_violation(cmd, None);
        assert!(why.map_or(false, |w| w.contains("leaves the workspace")), "{cmd}");
    }
}

#[test]
fn cd_inside_the_workspace_or_to_temp_is_fine() {
    let e = fake_home();
    for cmd in ["cd src && ls", "cd /tmp && ls", "cd ./src; pwd"] {
        assert_eq!(Workspace::new(&e.ws, &[]).shell_violation(cmd, None), None, "{cmd}");
    }
}

#[test]
fn secret_files_are_protected_in_every_spelling_but_templates_stay_readable() {
    let e = fake_home();
    let c = cfg(&e.ws);
    for cmd in ["git show HEAD:.env", "git show HEAD:apps/prod.env", "cat .npmrc", "cat deploy/credentials.json", "cat .env.staging"] {
        assert!(violation("shell", &json!({"command": cmd}), &c).is_some(), "{cmd}");
    }
    for path in [".env.example", "apps/.env.sample", "src/environment.py"] {
        assert!(violation("read_file", &json!({"path": path}), &c).is_none(), "{path}");
    }
}

#[test]
fn os_sandbox_hides_workspace_secrets_from_any_command() {
    if !sandbox::available() {
        return; // needs macOS sandbox-exec
    }
    let e = fake_home();
    let ws = &e.ws;
    fs::write(ws.join(".env"), "API_KEY=sk-SECRET\n").unwrap();
    fs::write(ws.join(".env.example"), "API_KEY=\n").unwrap();
    fs::write(ws.join("src/prod.env"), "TOKEN=sk-SECRET\n").unwrap();
    let c = cfg(ws);
    for script in ["grep -r KEY .", "cat .e*", "cat $(printf .en)v", "python3 -c 'print(open(\".env\").read())'", "cat src/prod.env"] {
        let r = sandboxed(script, &c, ws);
        let all = format!("{}{}", String::from_utf8_lossy(&r.stdout), String::from_utf8_lossy(&r.stderr));
        assert!(!all.contains("SECRET"), "{script}");
    }
    let argv = sandbox::wrap(vec!["/bin/cat".into(), ".env.example".into()], &c);
    let r = Command::new(&argv[0]).args(&argv[1..]).current_dir(ws).output().unwrap();
    assert!(r.status.success() && String::from_utf8_lossy(&r.stdout).contains("API_KEY="));
}

#[test]
fn shell_block_on_a_skill_folder_points_to_the_read_tools() {
    let e = fake_home();
    // another folder under the fake home
    let skills = e.home.join("elsewhere/skills/review-pr");
    fs::create_dir_all(&skills).unwrap();
    fs::write(skills.join("SKILL.md"), "---\nname: review-pr\ndescription: x\n---\nbody\n").unwrap();
    let mut c = cfg(&e.ws);
    c.skill_dirs = vec![skills.parent().unwrap().to_string_lossy().to_string()];
    let why = violation("shell", &json!({"command": format!("ls {}", skills.display())}), &c).unwrap();
    assert!(why.contains("read_file"), "{why}");
    // unrelated folders get no hint
    let other = violation("shell", &json!({"command": format!("ls {}", e.home.join("elsewhere").display())}), &c).unwrap();
    assert!(!other.contains("read_file"), "{other}");
}

#[test]
fn a_tilde_inside_prose_is_not_the_home_folder() {
    let e = fake_home();
    let w = Workspace::new(&e.ws, &[]);
    assert_eq!(w.shell_violation("audit the button (~line 97) and ~12 others", None), None);
    assert!(w.shell_violation("cat ~/secret", None).is_some());
    assert!(w.shell_violation("ls ~", None).is_some());
}

#[test]
fn a_commit_message_is_prose_not_a_path() {
    let e = fake_home();
    let w = Workspace::new(&e.ws, &[]);
    assert_eq!(w.shell_violation("git commit -m 'Add rules from ~ and /Users/x/notes'", None), None);
    assert_eq!(w.shell_violation("git commit --message=\"read ~/.melon first\"", None), None);
    // a real path after the message still counts
    assert!(w.shell_violation("git commit -m 'x' && cat /etc/hosts", None).is_some());
}

#[cfg(target_os = "macos")]
#[test]
fn git_commit_works_beside_a_tracked_secret_file_that_stays_unreadable() {
    // lint-staged and git stat every tracked file; a denied stat on apps/web/.env.test failed every commit in tunyt
    if !sandbox::available() {
        return;
    }
    let e = fake_home();
    let p = &e.ws;
    let git = |a: &[&str]| Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(a).current_dir(p).output().unwrap();
    git(&["init", "-q"]);
    fs::write(p.join(".env.test"), "SECRET=1").unwrap();
    fs::write(p.join("a.txt"), "a").unwrap();
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::OpenOptions::new().write(true).open(p.join(".env.test")).unwrap().set_modified(hour_ago).unwrap();
    git(&["add", "-Af"]);
    git(&["commit", "-qm", "init"]);
    // past git's same-second "racy" window, as for any file not touched just now
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(p.join("a.txt"), "b").unwrap();
    let c = cfg(p);
    assert!(sandboxed("git stash -q && git stash pop -q && git -c user.name=t -c user.email=t@t commit -qam next", &c, p).status.success());
    assert!(!sandboxed("cat .env.test", &c, p).status.success());
}
