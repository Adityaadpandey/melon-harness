"""File discovery and text search shared by grep/glob.

Discovery asks git first (`git ls-files -co --exclude-standard`), so .gitignore
is respected, dotfiles like .github/ are found, and node_modules/dist never
flood the results. Outside git it walks the tree with sensible skips. Files
that look like secrets are never listed or searched, whatever the repo says."""
from __future__ import annotations

import fnmatch
import os
import re
import subprocess
from pathlib import Path, PurePosixPath

SECRET_GLOBS = (".env", ".env.*", "*.pem", "*.key", "id_rsa*", "id_ed25519*")
SKIP_DIRS = {"node_modules", "__pycache__", ".git", ".venv", "venv", "dist", "build", ".next", ".turbo", "target", ".pytest_cache"}
MAX_FILE_BYTES = 2_000_000
MAX_LINE_CHARS = 300


def _is_secret(name: str) -> bool:
    low = name.lower()
    return any(fnmatch.fnmatch(low, g) for g in SECRET_GLOBS)


def _is_binary(path: Path) -> bool:
    try:
        with open(path, "rb") as f:
            return b"\0" in f.read(4096)
    except OSError:
        return True


def list_files(root: Path, forbidden: list[str] | None = None) -> list[Path]:
    """Every searchable file under root, relative order stable (sorted)."""
    from safety.guard import is_forbidden_path

    forbidden = forbidden or []
    paths: list[Path] = []
    try:
        r = subprocess.run(
            ["git", "ls-files", "-co", "--exclude-standard", "-z"],
            cwd=root, capture_output=True, timeout=20,
        )
        if r.returncode == 0:
            paths = [root / p for p in r.stdout.decode("utf-8", "replace").split("\0") if p]
    except (OSError, subprocess.SubprocessError):
        pass
    if not paths:
        for dirpath, dirs, files in os.walk(root):
            dirs[:] = [d for d in dirs if d not in SKIP_DIRS]
            paths += [Path(dirpath) / f for f in files]

    out = []
    for p in paths:
        rel = p.relative_to(root)
        if _is_secret(p.name) or SKIP_DIRS & set(rel.parts[:-1]) or is_forbidden_path(rel, forbidden):
            continue
        try:
            if not p.is_file() or p.stat().st_size > MAX_FILE_BYTES:
                continue
        except OSError:
            continue
        out.append(p)
    return sorted(out)


def glob_match(rel: str, pattern: str) -> bool:
    """`*.py` matches by file name anywhere; patterns with a slash match the
    whole relative path and support `**`."""
    if "/" not in pattern:
        return fnmatch.fnmatch(PurePosixPath(rel).name, pattern)
    return PurePosixPath(rel).full_match(pattern)


def search(
    files: list[Path], root: Path, regex: re.Pattern, *, context: int = 0, limit: int = 100,
    glob: str | None = None, files_only: bool = False,
) -> tuple[list[str], int, int]:
    """(output lines, total matches, files with matches). Stops reading once
    `limit` matches are collected but keeps counting so the caller can say how
    much was cut."""
    out: list[str] = []
    total = hit_files = 0
    shown = 0
    for f in files:
        rel = f.relative_to(root).as_posix()
        if glob and not glob_match(rel, glob):
            continue
        if _is_binary(f):
            continue
        try:
            lines = f.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        hits = [i for i, line in enumerate(lines) if regex.search(line)]
        if not hits:
            continue
        hit_files += 1
        total += len(hits)
        if files_only:
            out.append(f"{rel} ({len(hits)})")
            continue
        if shown >= limit:
            continue
        keep: set[int] = set()
        for i in hits:
            if shown >= limit:
                break
            shown += 1
            keep.update(range(max(0, i - context), min(len(lines), i + context + 1)))
        hit_set = set(hits)
        prev = -2
        for i in sorted(keep):
            if context and i != prev + 1 and prev >= 0:
                out.append("--")
            text = lines[i] if len(lines[i]) <= MAX_LINE_CHARS else lines[i][:MAX_LINE_CHARS] + " …"
            out.append(f"{rel}{':' if i in hit_set else '-'}{i + 1}{':' if i in hit_set else '-'}{text}")
            prev = i
    return out, total, hit_files


def similar_paths(root: Path, missing: Path, forbidden: list[str] | None = None, limit: int = 5) -> str:
    """Hint for a guessed path: real files with the same name (or stem) under
    root, e.g. 'DescriptionText.tsx' -> 'apps/mobile/src/DescriptionText.tsx'."""
    try:
        files = list_files(root, forbidden)
    except Exception:
        return ""
    same = [f for f in files if f.name == missing.name] or [f for f in files if f.stem == missing.stem and missing.stem]
    if not same:
        return " Don't guess paths: locate it with glob or grep files_only first."
    shown = ", ".join(str(f.relative_to(root)) if f.is_relative_to(root) else str(f) for f in same[:limit])
    return f" Files with that name: {shown}"


# a repo path with at least one folder and an extension: apps/web/src/app/(event-detail)/[id]/Page.tsx, src/a.py:12
_PATH = re.compile(r"(?<![\w/.:])(/?(?:[\w.()\[\]@-]+/)+[\w.()\[\]@-]+\.[A-Za-z]{1,5})\b")


def check_paths(root: Path, text: str, forbidden: list[str] | None = None) -> str:
    """Notes on paths a sub-agent report names that are not on disk, with the real file of that name.
    A scout once reported apps/server/src/services/event-read.service.ts (it lives under services/event/) and a web
    EventInfoRows.tsx that only exists in apps/mobile; the main agent read both as written and lost a round."""
    missing = sorted({p for p in _PATH.findall(text) if not (Path(p) if p.startswith("/") else root / p).exists()})
    if not missing:
        return ""
    try:
        files = list_files(root, forbidden)
    except Exception:
        files = []
    rows = []
    for p in missing[:10]:
        same = [str(f.relative_to(root)) if f.is_relative_to(root) else str(f) for f in files if f.name == Path(p).name][:3]
        rows.append(f"- {p}: not on disk" + (f"; files with that name: {', '.join(same)}" if same else "; locate it with glob before reading"))
    return "Paths in this report that do not exist (checked by the harness; do not read them as written):\n" + "\n".join(rows)
