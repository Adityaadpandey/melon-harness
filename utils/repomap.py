"""A small, token-budgeted map of the repo for the system prompt: the files other files depend on most,
each with its top-level symbols. Example line: `agent/agent.py: Agent, _within, run`.

Files are ranked with PageRank over the import graph (an import is a vote for the imported file), so the
core modules come first and leaf scripts are dropped when the budget runs out."""
from __future__ import annotations

import ast
import re
import subprocess
from pathlib import Path

from utils.outline import outline

SOURCE = {".py", ".ts", ".tsx", ".js", ".jsx", ".go", ".rs"}
JS_IMPORT = re.compile(r"""(?:from|require\()\s*['"](\.[^'"]*)['"]""")
MAX_FILES = 400


def _files(cwd: Path) -> list[str]:
    try:
        out = subprocess.run(["git", "ls-files"], cwd=cwd, capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return []
    return [f for f in out.splitlines() if Path(f).suffix in SOURCE and "test" not in f.lower() and Path(f).name != "__init__.py"][:MAX_FILES]


def _imports(rel: str, src: str, known: set[str]) -> set[str]:
    found: set[str] = set()
    if rel.endswith(".py"):
        try:
            tree = ast.parse(src)
        except SyntaxError:
            return found
        names = [a.name for n in ast.walk(tree) if isinstance(n, ast.Import) for a in n.names]
        names += [n.module for n in ast.walk(tree) if isinstance(n, ast.ImportFrom) and n.module and not n.level]
        for name in names:
            parts = name.split(".")
            for i in range(len(parts), 0, -1):
                for cand in ("/".join(parts[:i]) + ".py", "/".join(parts[:i]) + "/__init__.py"):
                    if cand in known:
                        found.add(cand)
                        break
    else:
        base = Path(rel).parent
        for spec in JS_IMPORT.findall(src):
            target = (base / spec).as_posix()
            for ext in ("", ".ts", ".tsx", ".js", ".jsx", "/index.ts", "/index.js"):
                cand = Path(target + ext).as_posix()
                if cand in known:
                    found.add(cand)
                    break
    found.discard(rel)
    return found


def pagerank(links: dict[str, set[str]], rounds: int = 20, d: float = 0.85) -> dict[str, float]:
    nodes = list(links)
    n = len(nodes) or 1
    rank = {k: 1 / n for k in nodes}
    for _ in range(rounds):
        nxt = {k: (1 - d) / n for k in nodes}
        for k, outs in links.items():
            share = rank[k] / (len(outs) or n)
            for t in outs or nodes:
                nxt[t] += d * share
        rank = nxt
    return rank


def build(cwd: Path, budget_chars: int = 4000) -> str:
    files = _files(cwd)
    known = set(files)
    srcs: dict[str, str] = {}
    for f in files:
        try:
            srcs[f] = (cwd / f).read_text(encoding="utf-8", errors="replace")
        except OSError:
            pass
    if not srcs:
        return ""
    rank = pagerank({f: _imports(f, s, known) for f, s in srcs.items()})
    lines, used = [], 0
    for f in sorted(srcs, key=lambda k: -rank[k]):
        names = [s.name for s in outline(Path(f).name, srcs[f]) if s.indent == 0 and not s.name.startswith("_")][:8]
        line = f"{f}: {', '.join(names)}" if names else f
        if used + len(line) > budget_chars:
            break
        lines.append(line)
        used += len(line) + 1
    return "\n".join(lines)
