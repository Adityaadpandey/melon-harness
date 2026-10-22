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
from functools import lru_cache
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


_CAMEL = re.compile(r"([a-z0-9])([A-Z])")
_ACRONYM = re.compile(r"([A-Z]+)([A-Z][a-z])")
_WORD_BREAK = re.compile(r"[^a-z0-9]+")


@lru_cache(maxsize=1 << 16)  # a repo's vocabulary repeats: "props", "classes" are stemmed once, not per occurrence
def _stem(t: str) -> str:
    if len(t) > 4 and t.endswith("ies"):
        return t[:-3] + "y"
    if len(t) > 4 and t.endswith(("ches", "shes", "sses", "xes", "zes")):
        return t[:-2]
    if len(t) > 3 and t.endswith("s") and not t.endswith("ss"):
        return t[:-1]
    return t


def tokenize(text: str) -> list[str]:
    text = _ACRONYM.sub(r"\1 \2", _CAMEL.sub(r"\1 \2", text))
    return [_stem(t) for t in _WORD_BREAK.split(text.lower()) if t and t not in STOP]


def _index(root: Path, f: Path):
    """Term counts for one file, cached by mtime (in memory and on disk between runs)."""
    key = (str(root), str(f))
    try:
        st = f.stat()
    except OSError:
        return None
    mtime = st.st_mtime
    hit = _cache.get(key)
    if hit and hit[0] == mtime:
        return hit
    if f.suffix in SKIP_SUFFIX or st.st_size > MAX_FILE_BYTES:
        return None
    try:
        raw = f.read_bytes()
    except OSError:
        return None
    if b"\0" in raw[:2048]:
        return None
    counts = Counter(tokenize(raw.decode("utf-8", errors="replace")))  # whole file at once: line breaks split words anyway
    rel = tokenize(f.relative_to(root).as_posix())
    for t in rel:
        counts[t] += PATH_WEIGHT
    entry = (mtime, counts, sum(counts.values()))
    _cache[key] = entry
    return entry


def rank_files(root: Path, files: list[Path], query: str, limit: int = 8, snippets: int = 3, prior: dict[str, float] | None = None):
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
        boost = 1 + (prior or {}).get(f.relative_to(root).as_posix(), 0.0)  # files that fixed similar requests before
        out.append((s * (1 + 0.15 * sum(w for w, _ in top)) * boost, f, top))
    out.sort(key=lambda x: -x[0])
    result = []
    for s, f, top in out[:limit]:
        lines = f.read_text(encoding="utf-8", errors="replace").splitlines()
        result.append((f.relative_to(root).as_posix(), round(s, 1), [(i + 1, lines[i].strip()[:160]) for _, i in sorted(top, key=lambda x: x[1])]))
    return result


def warm(root: Path, forbidden: list[str] | None = None) -> None:
    """Build or refresh the index for a whole repo in a background thread at startup, so the first `search` of a
    session finds it ready (a cold index on Tunyt took 5.6s). Daemon thread: quitting never waits for it."""
    import threading

    from utils.search import list_files

    def run() -> None:
        try:
            _load(root)
            before = len(_cache)
            for f in list_files(root, forbidden):
                _index(root, f)
            if len(_cache) != before:
                _save(root)
        except Exception:
            pass  # warming is a head start, never a failure

    threading.Thread(target=run, daemon=True, name="melon-index-warm").start()


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
