"""Who imports what, read straight from the working tree: no snapshot to go stale, no external tool.

Resolves relative imports, tsconfig/jsconfig `paths` aliases ("@/components/Nav" -> src/components/Nav.tsx, the way
Next.js projects write almost every import) and Python imports, absolute and relative. Example:

    g = load(cwd)
    g.importers("apps/web/src/components/ui/DesktopNav.tsx")   # -> files that import it
    g.blast_radius(path)                                       # -> everything that depends on it, nearest first
"""
from __future__ import annotations

import ast
import json
import os
import re
import subprocess
from collections import defaultdict, deque
from pathlib import Path

SOURCE = {".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"}
JS_SPEC = re.compile(r"""(?:\bfrom|\bimport|\brequire\(|\bimport\()\s*['"]([^'"\n]+)['"]""")
JS_EXT = ("", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".json", "/index.ts", "/index.tsx", "/index.js", "/index.jsx")
TEST_HINT = re.compile(r"(^|/)(__tests__|tests?|e2e|spec)(/|$)|\.(test|spec)\.[jt]sx?$|(^|/)test_[^/]+\.py$|_test\.py$")
MAX_FILES = 8000

_CACHE: dict[str, tuple[tuple, "ImportGraph"]] = {}


def _tracked(cwd: Path) -> list[str]:
    try:
        out = subprocess.run(["git", "ls-files", "--cached", "--others", "--exclude-standard"], cwd=cwd, capture_output=True, text=True, timeout=20).stdout
    except (OSError, subprocess.SubprocessError):
        return []
    return [f for f in out.splitlines() if Path(f).suffix in SOURCE][:MAX_FILES]


def _jsonc(path: Path) -> dict:
    """tsconfig allows comments and trailing commas; strip them before json.loads."""
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return {}
    text = re.sub(r'("(?:\\.|[^"\\])*")|//[^\n]*|/\*.*?\*/', lambda m: m.group(1) or "", text, flags=re.S)
    text = re.sub(r",(\s*[}\]])", r"\1", text)
    try:
        return json.loads(text)
    except ValueError:
        return {}


class Aliases:
    """`paths` entries of the nearest tsconfig.json/jsconfig.json above a file, as (prefix, [targets relative to the repo])."""

    def __init__(self, cwd: Path):
        self.rules: dict[str, list[tuple[str, list[str]]]] = {}  # config dir (repo-relative) -> rules
        for cfg in ("tsconfig.json", "jsconfig.json"):
            for found in self._configs(cwd, cfg):
                conf = _jsonc(found).get("compilerOptions", {})
                base = (found.parent / conf.get("baseUrl", ".")).resolve()
                rules = []
                for pat, targets in (conf.get("paths") or {}).items():
                    prefix = pat.split("*")[0]
                    rules.append((prefix, [self._target(cwd, base, t) for t in targets]))
                if conf.get("baseUrl"):
                    rules.append(("", [self._target(cwd, base, "*")]))  # bare imports resolve against baseUrl
                self.rules[self._rel(cwd, found.parent.resolve())] = rules

    @classmethod
    def _target(cls, cwd: Path, base: Path, t: str) -> str:
        """"src/*" -> "src/*" relative to the repo; a pattern without * stays a plain path."""
        head = cls._rel(cwd, base / t.split("*")[0])
        return (head + "/*").lstrip("/") if "*" in t and head != "" else ("*" if "*" in t else head)

    @staticmethod
    def _configs(cwd: Path, name: str) -> list[Path]:
        try:
            out = subprocess.run(["git", "ls-files", "--cached", "--others", "--exclude-standard", f"*{name}"], cwd=cwd, capture_output=True, text=True, timeout=20).stdout
        except (OSError, subprocess.SubprocessError):
            return []
        return [cwd / p for p in out.splitlines() if Path(p).name == name]

    @staticmethod
    def _rel(cwd: Path, p: Path) -> str:
        try:
            rel = p.relative_to(cwd.resolve()).as_posix()
            return "" if rel == "." else rel
        except ValueError:
            return p.as_posix()

    def for_file(self, rel: str) -> list[tuple[str, list[str]]]:
        folder = Path(rel).parent
        for d in [folder, *folder.parents]:
            key = "" if str(d) == "." else d.as_posix()
            if key in self.rules:
                return self.rules[key]
        return []


def _resolve(base: str, known: set[str]) -> str | None:
    base = base.rstrip("/")
    for ext in JS_EXT:
        cand = Path(base + ext).as_posix()
        if cand in known:
            return cand
    return None


def _py_targets(rel: str, src: str, known: set[str]) -> set[str]:
    try:
        tree = ast.parse(src)
    except SyntaxError:
        return set()
    names: list[str] = []
    for n in ast.walk(tree):
        if isinstance(n, ast.Import):
            names += [a.name for a in n.names]
        elif isinstance(n, ast.ImportFrom):
            if n.level:
                pkg = Path(rel).parent
                for _ in range(n.level - 1):
                    pkg = pkg.parent
                mod = (pkg / n.module.replace(".", "/")).as_posix() if n.module else pkg.as_posix()
                names += [f"{mod}/{a.name}".lstrip("./").replace("/", ".") for a in n.names] + [mod.replace("/", ".")]
            elif n.module:
                names += [n.module] + [f"{n.module}.{a.name}" for a in n.names]  # `from pkg import mod` imports pkg/mod.py
    found = set()
    for name in names:
        parts = name.split(".")
        for i in range(len(parts), 0, -1):
            base = "/".join(parts[:i])
            hit = next((c for c in (base + ".py", base + "/__init__.py") if c in known), None)
            if hit:
                found.add(hit)
                break
    return found


class ImportGraph:
    def __init__(self, cwd: Path, files: list[str]):
        self.cwd = cwd
        self.files = files
        known = set(files)
        aliases = Aliases(cwd)
        self.out: dict[str, set[str]] = {f: set() for f in files}
        self.inc: dict[str, set[str]] = defaultdict(set)
        for f in files:
            try:
                src = (cwd / f).read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            targets = _py_targets(f, src, known) if f.endswith(".py") else self._js_targets(f, src, known, aliases)
            targets.discard(f)
            self.out[f] = targets
            for t in targets:
                self.inc[t].add(f)

    @staticmethod
    def _js_targets(rel: str, src: str, known: set[str], aliases: Aliases) -> set[str]:
        found = set()
        for spec in set(JS_SPEC.findall(src)):
            hit = None
            if spec.startswith("."):
                hit = _resolve(Path(os.path.normpath(Path(rel).parent / spec)).as_posix(), known)
            else:
                for prefix, targets in aliases.for_file(rel):
                    if prefix and not spec.startswith(prefix):
                        continue
                    rest = spec[len(prefix):]
                    for t in targets:
                        hit = _resolve(t.replace("*", rest) if "*" in t else t, known)
                        if hit:
                            break
                    if hit:
                        break
            if hit:
                found.add(hit)
        return found

    # ---- lookups
    def find_file(self, name: str) -> tuple[str | None, list[str]]:
        """A path, a path suffix or a bare file name -> the one file it means (or the candidates when it is ambiguous)."""
        name = name.strip().removeprefix("./")
        if name in self.out:
            return name, []
        hits = [f for f in self.files if f.endswith("/" + name) or f == name]
        if not hits:
            stem = [f for f in self.files if Path(f).stem == Path(name).stem]
            hits = stem
        return (hits[0], []) if len(hits) == 1 else (None, hits[:8])

    def blast_radius(self, rel: str, depth: int = 3, limit: int = 40) -> list[tuple[str, int]]:
        """Files that depend on `rel` directly or through others, nearest first: [(path, hops)]."""
        seen, queue, out = {rel: 0}, deque([rel]), []
        while queue:
            cur = queue.popleft()
            if seen[cur] >= depth:
                continue
            for dep in sorted(self.inc.get(cur, ())):
                if dep not in seen:
                    seen[dep] = seen[cur] + 1
                    out.append((dep, seen[dep]))
                    queue.append(dep)
        return out[:limit]

    def tests_for(self, rel: str) -> list[str]:
        stem = Path(rel).stem
        near = [f for f in self.files if TEST_HINT.search(f) and (Path(f).stem.replace(".test", "").replace(".spec", "") in (stem, f"test_{stem}") or stem in Path(f).stem)]
        via_import = [f for f, _ in self.blast_radius(rel, depth=2, limit=200) if TEST_HINT.search(f)]
        return sorted(set(near) | set(via_import))[:20]


def load(cwd: Path) -> ImportGraph:
    """Built once per working-tree state: rebuilt when the set of files or the newest modification time changes."""
    cwd = Path(cwd).resolve()
    files = _tracked(cwd)
    stamp = (len(files), max((_mtime(cwd / f) for f in files), default=0.0))
    cached = _CACHE.get(str(cwd))
    if cached and cached[0] == stamp:
        return cached[1]
    g = ImportGraph(cwd, files)
    _CACHE[str(cwd)] = (stamp, g)
    return g


def _mtime(p: Path) -> float:
    try:
        return p.stat().st_mtime
    except OSError:
        return 0.0
