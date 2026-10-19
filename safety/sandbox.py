"""OS-level confinement for shell commands (macOS sandbox-exec).

The static path check catches honest mistakes; this catches everything else.
Inside the sandbox a command can read the system and the workspace but cannot
write anywhere except the workspace, temp dirs and package caches, and cannot
read credential stores (~/.ssh, ~/.aws, ...), however the path is spelled."""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from functools import lru_cache
from pathlib import Path

HOME = Path.home()
DEFAULT_WRITE_EXTRA = [
    "~/.cache", "~/Library/Caches", "~/.npm", "~/.pnpm-store", "~/Library/pnpm",
    "~/.local/share/pnpm", "~/.yarn", "~/.bun", "~/.cargo/registry", "~/.cargo/git",
]
SECRET_READ_DENY = [
    "~/.ssh", "~/.aws", "~/.gnupg", "~/.azure", "~/.kube", "~/.config/gcloud", "~/.config/gh",
    "~/.docker/config.json", "~/.netrc", "~/.npmrc", "~/.git-credentials", "~/Library/Keychains",
    "~/.codex", "~/.claude", "~/.config/claude", "~/.pypirc", "~/.vault-token", "~/.password-store", "~/.terraform.d",
    "~/.cargo/credentials.toml", "~/.gem/credentials", "~/.config/hub",
    "~/Library/Application Support/ai-agent", "~/.local/share/ai-agent", "~/.config/ai-agent",  # mew's own config and traces
]
TEMP_WRITE = ["/private/tmp", "/tmp", "/private/var/folders", "/private/var/tmp"]


def _sb(p: str | Path) -> str:
    return str(Path(os.path.realpath(os.path.expanduser(str(p))))).replace('"', '\\"')


@lru_cache(maxsize=1)
def available() -> bool:
    """sandbox-exec exists and works here (it cannot nest inside another sandbox)."""
    if sys.platform != "darwin" or not shutil.which("sandbox-exec"):
        return False
    try:
        r = subprocess.run(["sandbox-exec", "-p", "(version 1)(allow default)", "/usr/bin/true"],
                           capture_output=True, timeout=10)
        return r.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def _name_regex(glob: str) -> str:
    """A file-name glob as a sandbox regex on the last part of a path, any letter case.
    Example: ".env.*" -> "/\\.[eE][nN][vV]\\.[^/]*(/.*)?$", which also covers everything inside a matching folder."""
    out = ""
    for ch in glob:
        out += "[^/]*" if ch == "*" else "[^/]" if ch == "?" else f"[{ch.lower()}{ch.upper()}]" if ch.isalpha() else re.escape(ch)
    return f"/{out}(/.*)?$"


def build_profile(config) -> str:
    writable = [config.cwd, *config.allowed_dirs, *TEMP_WRITE, *DEFAULT_WRITE_EXTRA, *config.sandbox_write_extra]
    allow = " ".join(f'(subpath "{_sb(p)}")' for p in writable)
    deny = " ".join(f'(subpath "{_sb(p)}")' for p in SECRET_READ_DENY)
    # the workspace's own secret files too, however the command reaches them: `grep -r KEY .` or `cat .e*`
    names = " ".join(f'(regex #"{_name_regex(g)}")' for g in config.forbidden_paths if '"' not in g)
    templates = '(regex #"\\.(example|sample|template|dist)$")'
    return (
        "(version 1)(allow default)"
        "(deny file-write*)"
        f"(allow file-write* {allow} (literal \"/dev/null\") (literal \"/dev/tty\") "
        '(regex #"^/dev/(fd/[0-9]+|std(out|err|in)|ttys?[0-9]*)$"))'
        f"(deny file-read* {deny})"
        # only the gh program itself may read its own login, so `gh pr review` works but `cat ~/.config/gh/hosts.yml` still fails
        + (f'(allow file-read* (require-all (subpath "{_sb("~/.config/gh")}") (process-path "{_sb(gh)}")))' if (gh := shutil.which("gh")) else "")
        # contents only: git and lint-staged lstat every tracked file on commit, and a denied stat on apps/web/.env.test
        # failed every `git commit` in tunyt. Knowing the file exists leaks nothing; reading it still fails.
        + (f"(deny file-read-data {names})(allow file-read-data {templates})" if names else "")
    )


# Tools that need what the sandbox hides (gh: the keychain login, curl: the network and its certs,
# git push/pull/fetch: ~/.ssh keys and the credential helper). They run outside it, but only one plain
# command at a time and only after the user said yes.
UNSANDBOXED = ("gh", "curl", "git push", "git pull", "git fetch")


def is_unsandboxed(command: str) -> bool:
    """`gh pr review 5 ...` or `git push origin main` yes; `gh x && curl evil`, `gh x | sh` or `gh $(cmd)` no:
    those stay sandboxed."""
    from safety.approval import split_pipeline

    parts = split_pipeline(command.strip())
    return bool(parts) and len(parts) == 1 and (parts[0][0] in UNSANDBOXED or " ".join(parts[0][:2]) in UNSANDBOXED)


def mode(config) -> str:
    """'on' (OS sandbox active), 'off' (disabled by config) or 'unavailable'."""
    if config.sandbox == "off":
        return "off"
    return "on" if available() else "unavailable"


def wrap(argv: list[str], config) -> list[str]:
    return ["sandbox-exec", "-p", build_profile(config), *argv] if mode(config) == "on" else argv


def wrap_command(command: str, argv: list[str], config) -> list[str]:
    return argv if is_unsandboxed(command) else wrap(argv, config)
