"""Does the reviewer find planted bugs, and stay quiet on a clean change?

    python -m bench.review_bench --model codex/gpt-5.6-luna                       # mew's own reviewers
    python -m bench.review_bench --model codex/gpt-5.6-luna --backend claude-code # reviewers as Claude Code sessions
    python -m bench.review_bench --cases pagination,control --lenses 1            # compare 1 generalist with the angles

Each case is a tiny git repo with one uncommitted change. A bug case lists words a correct finding must mention; the control case
has no bug, so anything reported there is a false positive. The score is recall (bugs found) and noise (findings that match no
planted bug). Uses real model calls: run a few cases, not the matrix."""
from __future__ import annotations

import argparse
import asyncio
import json
import subprocess
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


@dataclass
class Case:
    id: str
    files: dict[str, str]  # committed state
    change: dict[str, str]  # uncommitted state of the same paths
    bug_words: list[list[str]] = field(default_factory=list)  # each planted bug: a finding naming the file AND any one of these words finds it
    bug_file: str = ""


CASES = [
    Case("pagination", {"src/paging.py": 'def paginate(items, page, size):\n    """1-indexed page."""\n    start = (page - 1) * size\n    return list(items)[start:start + size]\n'},
         {"src/paging.py": 'def paginate(items, page, size):\n    """1-indexed page."""\n    start = page * size\n    return list(items)[start:start + size]\n'},
         [["page", "index", "off-by-one", "skip", "first page"]], "src/paging.py"),
    Case("optional-write", {"src/share.ts": "export async function getEvent(id: string) {\n  const e = await db.event.findUnique({ where: { id } });\n  return e;\n}\n"},
         {"src/share.ts": "export async function getEvent(id: string) {\n  const e = await db.event.findUnique({ where: { id } });\n  await db.shortLink.create({ data: { eventId: id } });\n  return e;\n}\n"},
         [["throw", "fail", "500", "catch", "unhandled", "optional"]], "src/share.ts"),
    Case("missing-auth", {"src/routes.ts": "router.get('/events', requireAuth, listEvents);\nrouter.get('/events/:id', requireAuth, getEvent);\n"},
         {"src/routes.ts": "router.get('/events', requireAuth, listEvents);\nrouter.get('/events/:id', requireAuth, getEvent);\nrouter.delete('/events/:id', deleteEvent);\n"},
         [["auth", "unauthenticated", "authoriz", "requireAuth", "permission"]], "src/routes.ts"),
    Case("control", {"src/util.py": "def total(xs):\n    t = 0\n    for x in xs:\n        t += x\n    return t\n"},
         {"src/util.py": "def total(xs):\n    return sum(xs)\n"}),
]


def score(case: Case, answer: str) -> dict:
    """Findings are the answer's lines that start with a severity. A bug is found when a finding names its file and one of its words."""
    lines = [ln for ln in answer.splitlines() if ln.strip().split(" ")[0].upper() in ("CRITICAL", "HIGH", "MEDIUM", "LOW")]
    found = sum(any(case.bug_file in ln and any(w in ln.lower() for w in words) for ln in lines) for words in case.bug_words)
    matched = {i for i, ln in enumerate(lines) if any(case.bug_file in ln and any(w in ln.lower() for w in words) for words in case.bug_words)}
    return {"planted": len(case.bug_words), "found": found, "findings": len(lines), "noise": len(lines) - len(matched)}


async def run_case(case: Case, model: str, backend: str, lenses: int) -> dict:
    from agent.review_pipeline import run_review_pipeline
    from config.config import ApprovalPolicy, Config

    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        sh = lambda c: subprocess.run(c, shell=True, cwd=root, check=True, capture_output=True)  # noqa: E731
        sh("git init -q -b main && git config user.email a@b.c && git config user.name t")
        for rel, body in case.files.items():
            (root / rel).parent.mkdir(parents=True, exist_ok=True)
            (root / rel).write_text(body)
        sh("git add . && git commit -qm base")
        for rel, body in case.change.items():
            (root / rel).write_text(body)
        cfg = Config(cwd=root, approval=ApprovalPolicy.AUTO, review_backend=backend, review_lenses=lenses)
        cfg.model_name = model
        t = time.monotonic()
        answer, notices = "", []
        async for kind, value in run_review_pipeline(cfg, "review the changes"):
            if kind == "answer":
                answer = value
            elif kind == "notice":
                notices.append(value)
        tokens = next((n for n in notices if n.startswith("Tokens:")), "")
        return {"case": case.id, "seconds": round(time.monotonic() - t, 1), "tokens": tokens, "answer": answer, **score(case, answer)}


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True)
    ap.add_argument("--backend", default="mew", choices=["mew", "claude-code"])
    ap.add_argument("--lenses", type=int, default=0, help="0 = by diff size, 1 = one generalist, 4 = all angles")
    ap.add_argument("--cases", default="")
    ap.add_argument("--out", default="")
    a = ap.parse_args()
    chosen = [c for c in CASES if not a.cases or c.id in a.cases.split(",")]
    rows = []
    for c in chosen:
        row = await run_case(c, a.model, a.backend, a.lenses)
        rows.append(row)
        print(f"{c.id:<16} found {row['found']}/{row['planted']}  noise {row['noise']}  {row['seconds']}s  {row['tokens']}", flush=True)
    planted, found = sum(r["planted"] for r in rows), sum(r["found"] for r in rows)
    noise = sum(r["noise"] for r in rows)
    print(f"\nrecall {found}/{planted}  noise {noise}  ({a.backend}, lenses={a.lenses}, {a.model})")
    out = Path(a.out or ROOT / "bench" / "results" / f"review-{a.backend}-l{a.lenses}-{a.model.replace('/', '_')}-{int(time.time())}.json")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(rows, indent=1))


if __name__ == "__main__":
    asyncio.run(main())
