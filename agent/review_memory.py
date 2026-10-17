"""What earlier reviews of this repo already ruled out, so the same wrong claim is not raised every time.

A claim the verifier refuted (or one the user called wrong) is kept per workspace, outside the repo, for two weeks:

    a.ts | write is unguarded | handled by the caller's try/catch at b.ts:12

The next review shows these to its reviewers as "already ruled out"; code that changed since then can still be reported again
because the entry names the file and the reason, and the reviewer reads the file."""
from __future__ import annotations

import hashlib
import json
import time
from pathlib import Path

from utils.paths import atomic_write

KEEP_DAYS = 14
MAX_ENTRIES = 15


def path_for(cwd: Path, base: Path | None = None) -> Path:
    if base is None:
        from config.loader import get_data_dir

        base = get_data_dir()
    return base / "review_memory" / (hashlib.sha1(str(Path(cwd).resolve()).encode()).hexdigest()[:16] + ".json")


def load(cwd: Path, base: Path | None = None) -> list[dict]:
    try:
        rows = json.loads(path_for(cwd, base).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return []
    fresh = time.time() - KEEP_DAYS * 86400
    return [r for r in rows if isinstance(r, dict) and r.get("at", 0) > fresh][-MAX_ENTRIES:]


def remember(cwd: Path, rejected: list[tuple[str, str, str]], base: Path | None = None) -> None:
    """rejected = (where, what, why). Same file and wording is stored once."""
    if not rejected:
        return
    rows = load(cwd, base)
    have = {(r["where"].split(":")[0], r["what"][:60]) for r in rows}
    for where, what, why in rejected:
        key = (where.split(":")[0], what[:60])
        if key not in have:
            rows.append({"where": where, "what": what[:160], "why": why[:160], "at": time.time()})
            have.add(key)
    try:
        p = path_for(cwd, base)
        p.parent.mkdir(parents=True, exist_ok=True)
        atomic_write(p, json.dumps(rows[-MAX_ENTRIES:]))
    except OSError:
        pass  # a memory that cannot be written costs one repeated claim, nothing more


def render(cwd: Path, base: Path | None = None) -> str:
    rows = load(cwd, base)
    if not rows:
        return ""
    lines = [f"- {r['where']}: {r['what']} (ruled out because: {r['why'] or 'the code contradicts it'})" for r in rows]
    return "ALREADY RULED OUT in earlier reviews of this repo (do not report these again unless the code has changed since):\n" + "\n".join(lines) + "\n\n"
