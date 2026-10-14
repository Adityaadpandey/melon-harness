"""Finding the project when mew was started one folder above it (in ~/Devlopment with ~/Devlopment/tunyt
inside). Outside a repo the harness cannot see what changed, so it skips checks, review and undo."""
from __future__ import annotations

import re
from collections import Counter
from pathlib import Path


def in_repo(cwd: Path) -> bool:
    return any((d / ".git").exists() for d in [cwd, *cwd.parents])


def child_repos(cwd: Path) -> list[Path]:
    try:
        return [d for d in sorted(cwd.iterdir()) if not d.name.startswith(".") and (d / ".git").exists()]
    except OSError:
        return []


def pick_root(cwd: Path, message: str, hint: Path | None = None, targets: list[str] = ()) -> Path | None:
    """The repo one level below `cwd` that this request is about, or None when cwd is already inside
    a repo or nothing says which one. The signals, in order: it is the only repo there, the request
    names one ("in tunyt", or a path inside it), files the agent already touched live in one, or the
    previous request used one."""
    if in_repo(cwd):
        return None
    repos = child_repos(cwd)
    if len(repos) == 1:
        return repos[0]
    named = [r for r in repos if re.search(rf"(?<![\w-]){re.escape(r.name)}(?![\w-])", message, re.I) or str(r) in message]
    if len(named) == 1:
        return named[0]
    seen = Counter(r for t in targets for r in repos if t.startswith(str(r) + "/"))
    if seen:
        return seen.most_common(1)[0][0]
    return hint if hint in repos else None
