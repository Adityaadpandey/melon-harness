"""Workspace confinement. The session lives in the folder it was started in;
file tools may not leave it (symlinks included), and shell commands may not
name paths in the user's home or other private areas outside it."""
from __future__ import annotations

import os
import re
from pathlib import Path

# outside the workspace these are private: other projects, dotfiles, credentials
SENSITIVE_ROOTS = ("/etc", "/private/etc", "/root", "/var/root", "/Users", "/home", "/Volumes", "/mnt", "/media")

# command text that names a path: ~, $HOME, absolute paths (not URLs), and ../ traversal
# `cd` and `pushd` with their target, which may be missing (plain `cd` goes home)
_CD_RE = re.compile(r"(?:^|[;&|(]|\b(?:then|do))\s*(?:cd|pushd)(?:\s+(?P<to>[^\s;&|)]+))?\s*(?=$|[;&|)])")
TEMP_DIRS = ("/tmp", "/private/tmp", "/var/folders", "/private/var/folders", "/var/tmp", "/private/var/tmp")

_PATH_RE = re.compile(
    r"""(?x)
    (?P<home>(?<![\w$])~(?![\w-])(?:/[^\s"';|&<>()`]*)?)  # "~line 97" in prose is not home
  | (?P<envhome>\$\{?HOME\}?(?:/[^\s"';|&<>()`]*)?)
  | (?P<abs>(?<![\w.:$/\-])/[^\s"';|&<>()`=,]+)
  | (?P<rel>(?<![\w/.\-])\.\.(?:/[^\s"';|&<>()`]*)?(?![\w.]))
    """
)


# the quoted message of `git commit -m "..."` (or tag/merge/stash): prose, not paths.
# "Add autoApprove rules from ~ and the repo" once got a commit blocked as reaching into home.
_GIT_MESSAGE = re.compile(r"""(\bgit\b[^;&|]*?\s(?:-m|--message)(?:\s+|=))("(?:[^"\\]|\\.)*"|'[^']*')""")


def _real(p: str | Path) -> Path:
    return Path(os.path.realpath(os.path.expanduser(str(p))))


class Workspace:
    def __init__(self, root: str | Path, extra: list[str] | None = None):
        self.root = _real(root)
        self.extra = [_real(e) for e in (extra or [])]
        self.home = _real(Path.home())
        # /home is a symlink on macOS (automount), so compare resolved forms
        self.sensitive = [_real(r) for r in SENSITIVE_ROOTS]

    @property
    def roots(self) -> list[Path]:
        return [self.root, *self.extra]

    def contains(self, path: str | Path, base: str | Path | None = None) -> bool:
        """True if `path` (relative to `base`, default the root) stays inside the
        workspace after resolving symlinks and `..`."""
        p = Path(os.path.expanduser(str(path)))
        if not p.is_absolute():
            p = Path(base or self.root) / p
        real = _real(p)
        return any(real == r or real.is_relative_to(r) for r in self.roots)

    def shell_violation(self, command: str, cwd: str | Path | None = None) -> str | None:
        """First path in `command` that points somewhere private outside the
        workspace. Static analysis only: the OS sandbox is the backstop for
        anything built dynamically ($(...), eval, encoded paths)."""
        base = _real(cwd or self.root)
        command = _GIT_MESSAGE.sub(r"\1''", command)
        for m in _CD_RE.finditer(command):
            # after `cd /` every relative path ("cat etc/passwd") means something else, so a shell
            # may only move inside the workspace or into a temp dir. Plain `cd` and `cd -` count as leaving.
            to = (m.group("to") or "~").strip("'\"")
            target = _real(base / os.path.expanduser(to.replace("$HOME", "~").replace("${HOME}", "~"))) if to != "-" else self.home
            inside = any(target == r or target.is_relative_to(r) for r in self.roots)
            # a temp dir is fine unless it holds home or the workspace (`cd /private/var` is not a scratch dir)
            temp = any(target.is_relative_to(_real(t)) for t in TEMP_DIRS) and not (
                self.home.is_relative_to(target) or self.root.is_relative_to(target))
            if not (inside or temp):
                return f"'cd {to}' leaves the workspace {self.root}; run the command from inside it"
        for m in _PATH_RE.finditer(command):
            raw = m.group(0)
            expanded = raw.replace("${HOME}", str(self.home)).replace("$HOME", str(self.home))
            candidate = Path(os.path.expanduser(expanded))
            if not candidate.is_absolute():
                candidate = base / candidate
            real = _real(os.path.normpath(candidate))
            if any(real == r or real.is_relative_to(r) for r in self.roots):
                continue
            private = real == self.home or real.is_relative_to(self.home) or any(
                real == r or real.is_relative_to(r) for r in self.sensitive
            )
            # the parent of the workspace is private too (".." from the root), unless it is a shared temp dir
            if private:
                return f"'{raw}' resolves to {real}, outside the workspace {self.root}"
        return None
