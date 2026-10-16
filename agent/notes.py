"""Task notes: what a long job has decided and done, kept on disk so it survives compaction, /resume and a new session.

The model keeps them with the `notes` tool, like a lab notebook:

    Goal: move the events list pages into the (list) route group
    Decided: URL stays the same; no content change
    Done: pages moved, imports fixed in 4 files
    Next: run check-types, then commit

Stored outside the repo (data dir), one file per workspace. A compaction summary and the next session's prompt both
carry them, so a 6-hour task does not forget its own plan."""
from __future__ import annotations

import hashlib
import time
from pathlib import Path

from utils.paths import atomic_write

MAX_CHARS = 3000
PROMPT_CHARS = 1500
STALE_DAYS = 14


def path_for(cwd: Path, base: Path | None = None) -> Path:
    if base is None:
        from config.loader import get_data_dir

        base = get_data_dir()
    return base / "notes" / (hashlib.sha1(str(Path(cwd).resolve()).encode()).hexdigest()[:16] + ".md")


def read(cwd: Path, base: Path | None = None) -> str:
    try:
        return path_for(cwd, base).read_text(encoding="utf-8").strip()
    except OSError:
        return ""


def write(cwd: Path, text: str, base: Path | None = None) -> None:
    p = path_for(cwd, base)
    if not text.strip():
        p.unlink(missing_ok=True)
        return
    p.parent.mkdir(parents=True, exist_ok=True)
    atomic_write(p, text.strip()[:MAX_CHARS] + "\n")


def age_days(cwd: Path, base: Path | None = None) -> float:
    try:
        return (time.time() - path_for(cwd, base).stat().st_mtime) / 86400
    except OSError:
        return 0.0


def prompt_section(cwd: Path, base: Path | None = None) -> str:
    """Notes from earlier work, for the start of a session. Old ones are dropped: a stale plan misleads more than it helps."""
    text = read(cwd, base)
    if not text or age_days(cwd, base) > STALE_DAYS:
        return ""
    days = age_days(cwd, base)
    when = "today" if days < 1 else f"{int(days)} day(s) ago"
    return (
        f"# Task notes (written {when}, still yours to update)\n\n{text[:PROMPT_CHARS]}\n\n"
        "These come from earlier work in this repo. If the request is about something else, ignore them and clear them with the notes tool."
    )
