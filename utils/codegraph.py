"""Read-only access to a graphify code graph (graphify-out/graph.json).

graphify stores nodes (files, functions, classes, methods) and edges (calls,
contains, imports) with file:line. This answers structure questions in one
lookup instead of a chain of grep and read calls. The graph is a snapshot, so
callers must treat answers as pointers to verify, not as facts."""
from __future__ import annotations

import json
import subprocess
import time
from collections import defaultdict, deque
from pathlib import Path

_CACHE: dict[tuple[str, float], "CodeGraph"] = {}


def locate(cwd: Path) -> Path | None:
    for d in [cwd.resolve(), *cwd.resolve().parents][:6]:
        f = d / "graphify-out" / "graph.json"
        if f.is_file():
            return f
    return None


def load(cwd: Path) -> "CodeGraph | None":
    path = locate(cwd)
    if not path:
        return None
    key = (str(path), path.stat().st_mtime)
    if key not in _CACHE:
        _CACHE.clear()  # one graph at a time keeps memory bounded
        _CACHE[key] = CodeGraph(path, cwd)
    return _CACHE[key]


def _norm(label: str) -> str:
    return label.strip().lstrip(".").removesuffix("()").lower()


def kind_of(node: dict) -> str:
    label = node.get("label", "")
    if label.endswith("()"):
        return "method" if label.startswith(".") else "function"
    if "." in label and "/" not in label and not label.startswith("."):
        return "file"
    return "class" if label[:1].isupper() else "symbol"


class CodeGraph:
    def __init__(self, path: Path, cwd: Path):
        data = json.loads(path.read_text())
        self.file, self.workspace = path, cwd.resolve()
        self.built = path.stat().st_mtime
        self.nodes: dict[str, dict] = {n["id"]: n for n in data["nodes"]}
        self.out: dict[str, list[tuple[str, dict]]] = defaultdict(list)  # id -> [(other, edge)] as source
        self.inc: dict[str, list[tuple[str, dict]]] = defaultdict(list)  # id -> [(other, edge)] as target
        for e in data["links"]:
            s, t = e.get("source"), e.get("target")
            if s in self.nodes and t in self.nodes:
                self.out[s].append((t, e))
                self.inc[t].append((s, e))
        self.by_label: dict[str, list[str]] = defaultdict(list)
        for nid, n in self.nodes.items():
            self.by_label[_norm(n["label"])].append(nid)
        self._rel: dict[str, str] = {}

    # ---- locations
    def rel(self, source_file: str) -> str:
        """Graph paths are absolute from wherever it was built; map them onto this workspace
        by finding the longest suffix that exists here."""
        if source_file in self._rel:
            return self._rel[source_file]
        parts = Path(source_file).parts
        found = source_file
        for i in range(1, len(parts)):
            cand = Path(*parts[i:])
            if (self.workspace / cand).exists() or any((p / cand).exists() for p in [self.workspace, *self.workspace.parents][:3]):
                found = cand.as_posix()
                break
        self._rel[source_file] = found
        return found

    def where(self, nid: str) -> str:
        n = self.nodes[nid]
        return f"{self.rel(n.get('source_file', ''))}:{str(n.get('source_location', '')).lstrip('L')}"

    def degree(self, nid: str) -> int:
        return len(self.out[nid]) + len(self.inc[nid])

    # ---- lookups
    def find(self, query: str, limit: int = 15) -> list[str]:
        q = _norm(query)
        scored = []
        for nid, n in self.nodes.items():
            label = _norm(n["label"])
            if label == q:
                score = 0
            elif label.startswith(q):
                score = 1
            elif q in label:
                score = 2
            elif q in n.get("source_file", "").lower():
                score = 3
            else:
                continue
            scored.append((score, -self.degree(nid), nid))
        scored.sort()
        return [nid for _, _, nid in scored[:limit]]

    def resolve(self, name: str) -> tuple[str | None, list[str]]:
        """Best node for a name, plus other candidates when it is ambiguous."""
        hits = self.find(name, 8)
        exact = [h for h in hits if _norm(self.nodes[h]["label"]) == _norm(name)]
        pool = exact or hits
        return (pool[0] if pool else None), pool[1:5]

    def path(self, a: str, b: str, max_depth: int = 8) -> list[str] | None:
        seen, queue = {a: None}, deque([a])
        while queue:
            cur = queue.popleft()
            if cur == b:
                out = []
                while cur is not None:
                    out.append(cur)
                    cur = seen[cur]
                return out[::-1]
            if len(seen) > 200_000:
                break
            for other, _ in self.out[cur] + self.inc[cur]:
                if other not in seen:
                    seen[other] = cur
                    queue.append(other)
        return None

    def community(self, cid) -> list[str]:
        return sorted((nid for nid, n in self.nodes.items() if n.get("community") == cid), key=lambda x: -self.degree(x))

    # ---- freshness
    def staleness(self) -> str | None:
        try:
            r = subprocess.run(["git", "log", "--since", time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(self.built)), "--oneline"],
                               cwd=self.workspace, capture_output=True, text=True, timeout=10)
            n = len([ln for ln in r.stdout.splitlines() if ln.strip()])
        except (OSError, subprocess.SubprocessError):
            return None
        age = time.strftime("%Y-%m-%d", time.localtime(self.built))
        return f"graph built {age}; {n} commit(s) since then, so recent changes may be missing" if n else None
