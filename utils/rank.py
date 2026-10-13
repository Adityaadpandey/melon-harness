"""Ranked code search: BM25 over file paths and contents.

Idea taken from pi's tool search: tokenize identifiers at camelCase and
snake_case boundaries, drop stop words, stem plurals, then rank with Okapi BM25.
Here the documents are files, so "event description blank line section" lands
on EventAbout.tsx and description-markdown.ts without a 236-match grep first.
A path hit counts three times, since file names say what a file is for."""
from __future__ import annotations

import math
import re
from collections import Counter
from pathlib import Path

STOP = {"a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "of", "on", "or",
        "that", "the", "this", "to", "with", "when", "if", "no", "not", "we", "do", "does"}
MAX_FILE_BYTES = 200_000
SKIP_SUFFIX = {".lock", ".svg", ".map", ".min.js", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".woff", ".woff2",
               ".pdf", ".zip", ".snap", ".csv"}
PATH_WEIGHT = 3
# docs, specs, one-off scripts and tests describe code but are rarely the thing to fix
NOISE = re.compile(r"(^|/)(graphify-out|infrastructure|docs?|specs?|scripts|blog|content|fixtures|__tests__|tests?|unit|e2e|migrations)/|\.(md|mdx|spec\.\w+|test\.\w+)$")
NOISE_FACTOR = 0.35
_cache: dict[tuple[str, str], tuple[float, Counter, int]] = {}
_loaded: set[str] = set()


def _stem(t: str) -> str:
    if len(t) > 4 and t.endswith("ies"):
        return t[:-3] + "y"
    if len(t) > 4 and re.search(r"(ches|shes|sses|xes|zes)$", t):
        return t[:-2]
    if len(t) > 3 and t.endswith("s") and not t.endswith("ss"):
        return t[:-1]
    return t


def tokenize(text: str) -> list[str]:
    text = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", text)
    text = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1 \2", text)
    return [_stem(t) for t in re.split(r"[^a-z0-9]+", text.lower()) if t and t not in STOP]


def _index(root: Path, f: Path):
    """Term counts for one file, cached by mtime (in memory and on disk between runs)."""
    key = (str(root), str(f))
    try:
        mtime = f.stat().st_mtime
    except OSError:
        return None
    hit = _cache.get(key)
    if hit and hit[0] == mtime:
        return hit
    try:
        if f.suffix in SKIP_SUFFIX or f.stat().st_size > MAX_FILE_BYTES:
            return None
        raw = f.read_bytes()
    except OSError:
        return None
    if b"\0" in raw[:2048]:
        return None
    lines = raw.decode("utf-8", errors="replace").splitlines()
    counts: Counter = Counter()
    for line in lines:
        counts.update(tokenize(line))
    rel = tokenize(f.relative_to(root).as_posix())
    for t in rel:
        counts[t] += PATH_WEIGHT
    entry = (mtime, counts, sum(counts.values()))
    _cache[key] = entry
    return entry


def rank_files(root: Path, files: list[Path], query: str, limit: int = 8, snippets: int = 3):
    """[(rel path, score, [(line no, text)])], best first."""
    q = list(dict.fromkeys(tokenize(query)))
    if not q:
        return []
    _load(root)
    before = len(_cache)
    docs = [(f, e) for f in files if (e := _index(root, f))]
    if len(_cache) != before and len(docs) >= 200:  # small trees rebuild instantly; not worth a file
        _save(root)
    if not docs:
        return []
    avg = sum(e[2] for _, e in docs) / len(docs) or 1
    idf = {t: math.log(1 + (len(docs) - n + 0.5) / (n + 0.5)) for t in q if (n := sum(1 for _, e in docs if t in e[1]))}
    k1, b = 1.2, 0.75
    scored = []
    for f, (_, counts, length) in docs:
        s = 0.0
        for t, w in idf.items():
            c = counts.get(t)
            if c:
                s += w * c * (k1 + 1) / (c + k1 * (1 - b + b * length / avg))
        if s > 0:
            # files that match more distinct query terms beat files that repeat one
            s *= 1 + 0.5 * sum(1 for t in idf if t in counts)
            if NOISE.search(f.relative_to(root).as_posix()):
                s *= NOISE_FACTOR
            scored.append((s, f))
    scored.sort(key=lambda x: -x[0])
    # rerank the leaders by co-occurrence: a line that holds several query terms
    # ("blank lines ... disappear on render") says more than the same terms spread over a file
    out = []
    for s, f in scored[:30]:
        line_tokens = [tokenize(line) for line in f.read_text(encoding="utf-8", errors="replace").splitlines()]
        per_line = [(sum(idf[t] for t in idf if t in set(toks)), i) for i, toks in enumerate(line_tokens)]
        per_line.sort(key=lambda x: (-x[0], x[1]))
        top = [x for x in per_line[:snippets] if x[0] > 0]
        out.append((s * (1 + 0.15 * sum(w for w, _ in top)), f, top))
    out.sort(key=lambda x: -x[0])
    result = []
    for s, f, top in out[:limit]:
        lines = f.read_text(encoding="utf-8", errors="replace").splitlines()
        result.append((f.relative_to(root).as_posix(), round(s, 1), [(i + 1, lines[i].strip()[:160]) for _, i in sorted(top, key=lambda x: x[1])]))
    return result


def _store(root: Path) -> Path:
    import hashlib
    from config.loader import get_data_dir

    return get_data_dir() / "search" / (hashlib.sha1(str(root).encode()).hexdigest()[:16] + ".pkl")


def _load(root: Path) -> None:
    """Warm the in-memory index from the last run; entries are checked against mtime on use."""
    import pickle

    if str(root) in _loaded:
        return
    _loaded.add(str(root))
    try:
        _cache.update(pickle.loads(_store(root).read_bytes()))
    except Exception:
        pass  # no cache yet, or an unreadable one: rebuild


def _save(root: Path) -> None:
    import pickle

    try:
        mine = {k: v for k, v in _cache.items() if k[0] == str(root)}
        target = _store(root)
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp = target.with_suffix(".tmp")
        tmp.write_bytes(pickle.dumps(mine, protocol=pickle.HIGHEST_PROTOCOL))
        tmp.replace(target)
    except Exception:
        pass  # a failed cache write must never fail a search
