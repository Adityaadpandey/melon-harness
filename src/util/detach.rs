use tokio::process::Command;

/// Starts the child detached from our terminal, as the leader of its own process group.
///
/// Unix: a new session (setsid). ssh and git ask for passphrases by opening /dev/tty directly, so a null stdin doesn't
/// stop them; without a controlling tty they fail straight away instead of drawing "Enter passphrase for key ..." over
/// the TUI. The child still leads its own group, so `platform::kill_tree(pid)` keeps working.
/// Windows: no console window flashes up per command, and Ctrl+C aimed at zerd doesn't hit the child.
pub fn detach(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        // SAFETY: setsid is async-signal-safe, which is all pre_exec allows between fork and exec
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    // a detached child has no controlling terminal (ps shows "?" or "??"), so ssh can't prompt on /dev/tty
    #[tokio::test]
    async fn child_has_no_controlling_tty() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("ps -o tty= -p $$");
        let out = detach(&mut cmd).output().await.unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).trim().starts_with('?'));
    }
}
