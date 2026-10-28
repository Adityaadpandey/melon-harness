use tokio::process::Command;

/// Starts the child in its own session, so it has no controlling terminal.
///
/// Why: ssh and git ask for passphrases by opening /dev/tty directly, so a null stdin doesn't stop them.
/// Without a tty they fail straight away instead of drawing "Enter passphrase for key ..." over the TUI.
/// Same effect as setsid, and the child is still its own process group leader, so killpg(pid) keeps working.
pub fn detach(cmd: &mut Command) -> &mut Command {
    // SAFETY: setsid is async-signal-safe, which is all pre_exec allows between fork and exec
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd
}

#[cfg(test)]
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
