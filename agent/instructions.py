"""Folder instructions that show up when the agent first touches that folder, like Claude Code's nested CLAUDE.md.

Reading apps/web/src/a.tsx surfaces, once per session, any AGENTS.md / CLAUDE.md in apps/web/src, apps/web, apps and the
repo root, plus a folder's Command Code taste (apps/web/.commandcode/taste/taste.md; the root's is already in the
system prompt). A file may pull in others with `@path` lines:

    # AGENTS.md
    Use pnpm. Run `pnpm check-types` before finishing.
    @docs/conventions.md            <- its text is inlined here (up to 3 levels deep, inside the repo or ~)
"""
from __future__ import annotations

import re
from pathlib import Path

from config.loader import taste_files, taste_text

NAMES = ("AGENTS.md", "CLAUDE.md")
IMPORT = re.compile(r"(?m)^\s*@([\w./~-]+\.(?:md|txt))\s*$")
MAX_DEPTH = 3
FILE_CHARS = 3000
TOTAL_CHARS = 6000


def _inline(text: str, base: Path, root: Path, depth: int, done: set[Path]) -> str:
    def swap(m: re.Match) -> str:
        target = Path(m.group(1)).expanduser()
        target = (target if target.is_absolute() else base / target).resolve()
        inside = target.is_relative_to(root.resolve()) or target.is_relative_to(Path.home().resolve())
        if depth >= MAX_DEPTH or target in done or not inside or not target.is_file():
            return m.group(0)
        done.add(target)
        try:
            body = target.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return m.group(0)
        return _inline(body, target.parent, root, depth + 1, done).strip()

    return IMPORT.sub(swap, text)


def for_path(root: Path, path: Path, seen: set[Path]) -> str:
    """Instruction text from folders between `path` and the repo root that have not been shown yet (outermost first)."""
    root = root.resolve()
    try:
        start = path.resolve().parent
        if start != root and root not in start.parents:
            return ""
    except OSError:
        return ""
    folders = [start, *start.parents]
    folders = [d for d in folders if d == root or root in d.parents][::-1]
    out, used = [], 0
    for d in folders:
        if d in seen:
            continue
        seen.add(d)
        for name in NAMES:
            f = d / name
            if not f.is_file():
                continue
            try:
                text = _inline(f.read_text(encoding="utf-8", errors="replace"), d, root, 0, {f.resolve()}).strip()
            except OSError:
                continue
            if text and used < TOTAL_CHARS:
                rel = f.relative_to(root).as_posix()
                out.append(f"## {rel}\n{text[:FILE_CHARS]}")
                used += min(len(text), FILE_CHARS)
            break  # AGENTS.md wins over CLAUDE.md in the same folder
        # outside the AGENTS.md budget: tunyt's two AGENTS.md files fill it, and taste files are a few lines each
        for f in taste_files(d) if d != root else []:
            if text := taste_text(f):
                out.append(f"## {f.relative_to(root).as_posix()} (learned taste, a default not a rule)\n{text[:FILE_CHARS]}")
    if not out:
        return ""
    return "[Instructions for this folder, shown once. Follow them for work here; they rank below the user's request.]\n\n" + "\n\n".join(out)
