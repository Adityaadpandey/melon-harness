//! The few places where Windows and Unix differ: which shell runs a command, how a process tree is killed,
//! file permissions and symlinks. Everything else in melon is portable, so the `cfg` noise stays in this file.
use std::path::{Path, PathBuf};

/// `[shell, "-c", command]` for the shell commands melon runs (tool calls, checks, hooks, project tools).
///
/// Unix: bash, or plain sh on a box without it (Alpine). Windows: Git Bash, because models write bash and git is
/// already a requirement. If Git for Windows isn't installed we fall back to PowerShell.
pub fn shell_argv(command: &str) -> Vec<String> {
    #[cfg(unix)]
    {
        let sh = if Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
        vec![sh.into(), "-c".into(), command.into()]
    }
    #[cfg(windows)]
    {
        match git_bash() {
            Some(b) => vec![b.to_string_lossy().into_owned(), "-c".into(), command.into()],
            None => vec!["powershell.exe".into(), "-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), command.into()],
        }
    }
}

/// Git for Windows ships bash next to git. We look there on purpose: a bare `bash` on PATH is often the WSL stub
/// in System32, which runs inside a Linux VM and can't see Windows paths the way the model wrote them.
#[cfg(windows)]
fn git_bash() -> Option<PathBuf> {
    use std::sync::OnceLock;
    static B: OnceLock<Option<PathBuf>> = OnceLock::new();
    B.get_or_init(|| {
        let mut roots: Vec<PathBuf> = Vec::new();
        // C:\Program Files\Git\cmd\git.exe -> C:\Program Files\Git
        if let Ok(git) = which::which("git") {
            if let Some(root) = git.parent().and_then(|p| p.parent()) {
                roots.push(root.to_path_buf());
            }
        }
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                roots.push(if var == "LOCALAPPDATA" { base.join("Programs").join("Git") } else { base.join("Git") });
            }
        }
        roots.into_iter().flat_map(|r| [r.join("bin").join("bash.exe"), r.join("usr").join("bin").join("bash.exe")]).find(|p| p.is_file())
    })
    .clone()
}

/// Make a path usable inside a bash command line: `C:\Users\me\x` becomes `C:/Users/me/x` (Git Bash accepts that).
pub fn bash_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Kills a process and everything it started. Unix: the whole process group (the child leads its own group, see
/// `detach`). Windows has no groups to signal, so `taskkill /T` walks the tree instead.
pub fn kill_tree(pid: u32) {
    // pid 0 would mean "our own group" to killpg
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::killpg(pid as i32, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(0x0800_0000).output();
    }
}

/// Owner-only permissions where the OS has them. Windows keeps files in the user's profile folder, which other
/// accounts can't read by default, so there is nothing to do there.
pub fn restrict(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(windows)]
    {
        let _ = (path, mode);
    }
}

/// A symlink to a directory. Windows needs a privilege for real symlinks, so there it is best effort.
pub fn symlink_dir(real: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(real, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(real, link)
    }
}

/// Exit code of a finished process; a process killed by a signal is reported as the negative signal number.
pub fn exit_code(st: &std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        st.code().unwrap_or_else(|| -(st.signal().unwrap_or(1)))
    }
    #[cfg(windows)]
    {
        st.code().unwrap_or(-1)
    }
}

/// Folders that are scratch space, so writing there is fine. Example on Windows: `C:\Users\me\AppData\Local\Temp`.
pub fn temp_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = if cfg!(windows) { vec![] } else { ["/tmp", "/private/tmp", "/var/folders", "/private/var/folders", "/var/tmp", "/private/var/tmp"].iter().map(PathBuf::from).collect() };
    v.push(std::env::temp_dir());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_argv_runs_the_command_as_one_argument() {
        let a = shell_argv("echo a && echo b");
        assert_eq!(a.last().unwrap(), "echo a && echo b");
    }

    #[test]
    fn bash_path_uses_forward_slashes() {
        assert_eq!(bash_path(Path::new(r"C:\Users\me\x.sh")), "C:/Users/me/x.sh");
    }
}
