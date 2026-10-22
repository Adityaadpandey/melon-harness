"""Understand a request before touching the repo: what kind of job it is, how hard, and where in THIS repo the answer probably lives.

One small call to the fast model reads the request next to a compact map of the repo (folders with file counts, what changed recently,
what is uncommitted) and answers in JSON. It replaces keyword lists: "review the change i have" is a review although "change" is an
edit word, and "where do we validate the invite code" points at folders nobody listed in a regex. Every part of this is optional:
when no fast model is configured, or the call fails, the agent falls back to the old regex rules, so nothing here can block a request."""
from __future__ import annotations

import json
import re
import subprocess
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path

from client.response import StreamEventType
from utils.search import list_files

INTENTS = ("change", "review", "investigate", "explain", "git", "other")
MAP_CHARS = 3500  # about 900 tokens: the whole map rides along with every request, so it stays small


@dataclass
class Orientation:
    intent: str = "other"
    tier: str = "default"
    where: list[str] = field(default_factory=list)
    queries: list[str] = field(default_factory=list)
    note: str = ""

    def hint(self) -> str:
        """The few lines the main agent gets. A guess from a map, so it is told to confirm before relying on it."""
        if not self.where and not self.queries:
            return ""
        parts = []
        if self.where:
            parts.append("likely in " + ", ".join(self.where[:5]))
        if self.queries:
            parts.append("start with search for " + " / ".join(f"'{q}'" for q in self.queries[:3]))
        return "[Orientation, a guess from a map of the repo (confirm by reading before relying on it): " + "; ".join(parts) + (f". {self.note}" if self.note else "") + "]"


def _git(cwd: Path, *args: str) -> str:
    try:
        r = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, timeout=10)
        return r.stdout if r.returncode == 0 else ""
    except (OSError, subprocess.SubprocessError):
        return ""


def repo_map(cwd: Path, forbidden: list[str] | None = None) -> str:
    """Folders (two levels deep) with their file counts, the top-level files, what the last commits touched and what is uncommitted."""
    files = [f.relative_to(cwd).as_posix() for f in list_files(cwd, forbidden)]
    folders: Counter = Counter()
    top: list[str] = []
    for rel in files:
        parts = rel.split("/")
        if len(parts) == 1:
            top.append(rel)
        else:
            folders["/".join(parts[:2]) if len(parts) > 2 else parts[0]] += 1
            if len(parts) > 2:
                folders[parts[0]] += 0  # keeps the parent visible even when only its children are counted
    tree = ", ".join(f"{d}/ ({n})" for d, n in folders.most_common(40) if n)
    recent = list(dict.fromkeys(l for l in _git(cwd, "log", "-8", "--name-only", "--pretty=format:").splitlines() if l.strip()))[:14]
    dirty = [l[3:] for l in _git(cwd, "status", "--short", "-uall").splitlines()][:10]
    text = f"FOLDERS: {tree}\nROOT FILES: {', '.join(top[:20])}\nRECENTLY CHANGED: {', '.join(recent) or '(none)'}\nUNCOMMITTED: {', '.join(dirty) or '(clean)'}"
    return text[:MAP_CHARS]


PROMPT = """You help a coding agent get oriented before it works. Read the request and the map of the repo, then reply with ONE JSON object and nothing else.

REQUEST: {request}

REPO MAP:
{repo}

Fields:
"intent": "change" (the user wants files edited or created), "review" (check code, a diff or a PR and report), "investigate" (find a cause or trace how something works), "explain" (answer a question about the code), "git" (commit, branch, push, status), or "other".
  Judge by what the user wants done, not by single words: "review the change i have" is a review; "why does checkout double charge" is investigate even though nothing is named.
"tier": "fast" (trivial and well-defined: rename, typo, where is X, run a command), "default" (ordinary work), or "strong" (hard: a bug with an unknown cause, concurrency, security or money paths, a change across several parts, a design decision).
"where": up to 5 paths or folders from the map where the answer most likely lives (only ones that appear in the map; [] if you cannot tell).
"queries": up to 3 plain-word search queries that would find the relevant code (identifiers, domain words the user used, synonyms a developer would have used in code); [] if the request names exact files.
"note": at most one short sentence: the one thing worth knowing before starting (for example that the uncommitted change is what the user means), or "".
"""


def parse(reply: str) -> Orientation | None:
    m = re.search(r"\{.*\}", reply or "", re.S)
    if not m:
        return None
    try:
        d = json.loads(m.group(0))
    except ValueError:
        return None
    if not isinstance(d, dict):
        return None
    intent = d.get("intent") if d.get("intent") in INTENTS else "other"
    tier = d.get("tier") if d.get("tier") in ("fast", "default", "strong") else "default"
    clean = lambda v: [str(x)[:120] for x in v if isinstance(x, str)][:5] if isinstance(v, list) else []
    return Orientation(intent, tier, clean(d.get("where")), clean(d.get("queries"))[:3], str(d.get("note") or "")[:200])


async def orient(client, role: str, request: str, repo: str) -> Orientation | None:
    """None when the call fails or the answer is not usable: the caller keeps its regex rules."""
    text = ""
    try:
        async for ev in client.chat_completion([{"role": "user", "content": PROMPT.format(request=request[:1500], repo=repo)}], stream=False, role=role):
            if ev.type == StreamEventType.MESSAGE_COMPLETE and ev.text_delta:
                text += ev.text_delta.content
            elif ev.type == StreamEventType.ERROR:
                return None
    except Exception:
        return None
    return parse(text)
