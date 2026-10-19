"""Replays the review of a fix on the tunyt co-host branch and scores it against what Claude Code found there.

    python -m bench.tunyt_fix_review            # needs ../tunyt in the state the session left it (10 modified files)

The author was told the findings below (the session's second reviewer), fixed them, and that fix is the working tree.
Claude Code's review of that tree is the answer key: a finding counts when it names the file and one of the words.
Uses real model calls (one review)."""
from __future__ import annotations

import asyncio
import json
import subprocess
import sys
from pathlib import Path

from agent.review import review_changes
from config.loader import load_config

TUNYT = Path(__file__).resolve().parent.parent.parent / "tunyt"
FIXING = "1. CheckInDoorsModal.tsx hides the list on a failed refetch.\n2. user.repository.ts non-admin search returns no match for email queries."
KEY = [  # (name, file, any of these words)
    ("refetch error wipes the list", "CheckInDoorsModal", ["refetch", "skeleton", "isError", "haseverloaded", "background"]),
    ("hasEverLoaded never reset", "CheckInDoorsModal", ["reset", "eventid", "never reset", "stays true"]),
    ("email substring match leaks who owns an address", "user.repository", ["substring", "prefix", "exact", "enumerat", "gmail", "contains"]),
    ("cohosts.service return shape changed", "cohosts.service", ["shape", "invited", "pending row", "return"]),
    ("dedup misses selected user typed by email", "CoHostSection", ["dedup", "seenemails", "duplicate", "two invites"]),
    ("userId path skips deactivated/self checks", "cohosts.service", ["deactivat", "yourself", "self", "owner"]),
    ("refine reports error on email only", "cohosts.types", ["refine", "path", "uuid"]),
]


async def main() -> None:
    files = [ln[3:] for ln in subprocess.run(["git", "status", "--porcelain"], cwd=TUNYT, capture_output=True, text=True).stdout.splitlines() if ln.startswith(" M")]
    cfg = load_config(TUNYT)
    res = await review_changes(cfg, "fix the review findings on the co-host dashboard", files, [], fixing=FIXING)
    text = res.findings.lower()
    rows = [(name, any(f.lower() in ln and any(w in ln for w in ws) for ln in text.splitlines())) for name, f, ws in KEY for f in [f]]
    for name, hit in rows:
        print("found  " if hit else "missed ", name)
    print(f"\nrecall {sum(h for _, h in rows)}/{len(KEY)}  verdict {res.verdict}\n\n{res.findings}")
    if "--json" in sys.argv:
        print(json.dumps(rows))


if __name__ == "__main__":
    asyncio.run(main())
