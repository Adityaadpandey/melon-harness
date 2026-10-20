"""File discovery and text search shared by grep/glob.

Discovery asks git first (`git ls-files -co --exclude-standard`), so .gitignore
is respected, dotfiles like .github/ are found, and node_modules/dist never
flood the results. Outside git it walks the tree with sensible skips. Files
that look like secrets are never listed or searched, whatever the repo says."""
from __future__ import annotations

import fnmatch
import os
import re
import stat
import subprocess
from pathlib import Path, PurePosixPath

SECRET_GLOBS = (".env", ".env.*", "*.pem", "*.key", "id_rsa*", "id_ed25519*")
SKIP_DIRS = {"node_modules", "__pycache__", ".git", ".venv", "venv", "dist", "build", ".next", ".turbo", "target", ".pytest_cache"}
MAX_FILE_BYTES = 2_000_000
MAX_LINE_CHARS = 300
_ODD_BREAKS = re.compile("[\r\x0b\x0c\x1c-\x1e\x85\u2028\u2029]")  # line breaks for splitlines() but not for re's $


def _is_secret(name: str) -> bool:
    low = name.lower()
    return any(fnmatch.fnmatch(low, g) for g in SECRET_GLOBS)


def list_files(root: Path, forbidden: list[str] | None = None) -> list[Path]:
    """Every searchable file under root, relative order stable (sorted)."""
    from safety.guard import is_forbidden_path

    forbidden = forbidden or []
    # plain "a/b/c.ts" strings until the end: on a 23k-file repo, Path objects were most of the cost
    rels: list[str] = []
    try:
        r = subprocess.run(
            ["git", "ls-files", "-co", "--exclude-standard", "-z"],
            cwd=root, capture_output=True, timeout=20,
        )
        if r.returncode == 0:
            rels = [p for p in r.stdout.decode("utf-8", "replace").split("\0") if p]
    except (OSError, subprocess.SubprocessError):
        pass
    if not rels:
        for dirpath, dirs, files in os.walk(root):
            dirs[:] = [d for d in dirs if d not in SKIP_DIRS]
            rel_dir = os.path.relpath(dirpath, root).replace(os.sep, "/")
            rels += [f if rel_dir == "." else f"{rel_dir}/{f}" for f in files]

    # names repeat a lot (23k files in a monorepo share a few hundred folder names), so each name is judged once
    bad: dict[str, bool] = {}

    def blocked(name: str) -> bool:
        if name not in bad:
            bad[name] = name in SKIP_DIRS or bool(is_forbidden_path(name, forbidden))
        return bad[name]

    base = str(root)
    out = []
    for rel in rels:
        *dirs, name = rel.split("/")
        if _is_secret(name) or any(blocked(d) for d in dirs) or (forbidden and is_forbidden_path(name, forbidden)):
            continue
        try:
            st = os.stat(os.path.join(base, rel))
        except OSError:
            continue
        if stat.S_ISREG(st.st_mode) and st.st_size <= MAX_FILE_BYTES:
            out.append(rel)
    out.sort(key=lambda rel: rel.split("/"))  # the order sorted() gives the Path objects: folder by folder
    return [root / rel for rel in out]


def glob_match(rel: str, pattern: str) -> bool:
    """`*.py` matches by file name anywhere; patterns with a slash match the
    whole relative path and support `**`."""
    if "/" not in pattern:
        return fnmatch.fnmatch(PurePosixPath(rel).name, pattern)
    return PurePosixPath(rel).full_match(pattern)


def search(
    files: list[Path], root: Path, regex: re.Pattern, *, context: int = 0, limit: int = 100,
    glob: str | None = None, files_only: bool = False, per_file: list[tuple[str, int]] | None = None,
) -> tuple[list[str], int, int]:
    """(output lines, total matches, files with matches). Stops reading once
    `limit` matches are collected but keeps counting so the caller can say how
    much was cut."""
    out: list[str] = []
    total = hit_files = 0
    shown = 0
    # One search over the whole file first: most files have no match and never get split into lines.
    # With MULTILINE, ^ and $ hold at "\n" just as they do at each line's ends, but splitlines() also breaks
    # at "\r" (CRLF files) and a few rarer characters, so in such files a pattern with ^, $ or a lookaround
    # goes line by line. \A and \Z mean line start/end per line but file start/end here: no shortcut at all.
    whole = None if re.search(r"\\[AZ]", regex.pattern) else re.compile(regex.pattern, regex.flags | re.MULTILINE)
    edgy = any(c in regex.pattern for c in ("^", "$", "(?"))
    for f in files:
        rel = f.relative_to(root).as_posix()
        if glob and not glob_match(rel, glob):
            continue
        try:
            raw = f.read_bytes()
        except OSError:
            continue
        if b"\0" in raw[:4096]:  # binary
            continue
        text = raw.decode("utf-8", errors="replace")
        if whole and not whole.search(text) and not (edgy and _ODD_BREAKS.search(text)):
            continue
        lines = text.splitlines()
        hits = [i for i, line in enumerate(lines) if regex.search(line)]
        if not hits:
            continue
        hit_files += 1
        total += len(hits)
        if per_file is not None:  # (path, matches) for every file, even past the limit
            per_file.append((rel, len(hits)))
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
