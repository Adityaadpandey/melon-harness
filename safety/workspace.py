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
_PATH_RE = re.compile(
    r"""(?x)
    (?P<home>(?<![\w$])~(?:/[^\s"';|&<>()`]*)?)
  | (?P<envhome>\$\{?HOME\}?(?:/[^\s"';|&<>()`]*)?)
  | (?P<abs>(?<![\w.:$/\-])/[^\s"';|&<>()`=,]+)
  | (?P<rel>(?<![\w/.\-])\.\.(?:/[^\s"';|&<>()`]*)?(?![\w.]))
    """
)


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
