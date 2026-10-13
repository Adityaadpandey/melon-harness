"""OS-level confinement for shell commands (macOS sandbox-exec).

The static path check catches honest mistakes; this catches everything else.
Inside the sandbox a command can read the system and the workspace but cannot
write anywhere except the workspace, temp dirs and package caches, and cannot
read credential stores (~/.ssh, ~/.aws, ...), however the path is spelled."""
from __future__ import annotations

import os
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


def build_profile(config) -> str:
    writable = [config.cwd, *config.allowed_dirs, *TEMP_WRITE, *DEFAULT_WRITE_EXTRA, *config.sandbox_write_extra]
    allow = " ".join(f'(subpath "{_sb(p)}")' for p in writable)
    deny = " ".join(f'(subpath "{_sb(p)}")' for p in SECRET_READ_DENY)
    return (
        "(version 1)(allow default)"
        "(deny file-write*)"
        f"(allow file-write* {allow} (literal \"/dev/null\") (literal \"/dev/tty\") "
        '(regex #"^/dev/(fd/[0-9]+|std(out|err|in)|ttys?[0-9]*)$"))'
        f"(deny file-read* {deny})"
    )


def mode(config) -> str:
    """'on' (OS sandbox active), 'off' (disabled by config) or 'unavailable'."""
    if config.sandbox == "off":
        return "off"
    return "on" if available() else "unavailable"


def wrap(argv: list[str], config) -> list[str]:
    return ["sandbox-exec", "-p", build_profile(config), *argv] if mode(config) == "on" else argv
