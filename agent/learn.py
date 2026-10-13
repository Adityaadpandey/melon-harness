"""What mew learns about a repo between sessions, kept outside the repo (data dir).

Only things that cannot be hallucinated go in:
  * lessons   - what YOU told it (a rejection with feedback, `/learn ...`). Shown in the
                system prompt, newest first, capped; `/lessons` lists them, `/forget N` removes one.
  * edit history - request words -> the files a verified run changed. `search` boosts files that
                fixed similar requests before, so it finds the right place in fewer calls.
Nothing the model merely claims is stored."""
from __future__ import annotations

import hashlib
import json
import time
from pathlib import Path

from utils.paths import atomic_write
from utils.rank import tokenize

MAX_LESSONS = 30
MAX_EDITS = 200
PROMPT_CHARS = 1500
MIN_OVERLAP = 0.25  # share of the request's words a past request must have in common


class Learned:
    def __init__(self, root: Path, base: Path | None = None):
        if base is None:
            from config.loader import get_data_dir

            base = get_data_dir()
        self.root = Path(root).resolve()
        self.path = base / "learned" / (hashlib.sha1(str(self.root).encode()).hexdigest()[:16] + ".json")
        self.data = {"lessons": [], "edits": []}
        try:
            loaded = json.loads(self.path.read_text())
            self.data.update({k: loaded.get(k, []) for k in self.data})
        except (OSError, ValueError):
            pass

    def _save(self) -> None:
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            atomic_write(self.path, json.dumps(self.data, indent=1))
        except OSError:
            pass  # learning is a bonus: never fail a run over it

    # lessons -----------------------------------------------------------------
    def add_lesson(self, text: str, source: str = "user") -> bool:
        text = " ".join(text.split())[:300]
        if len(text.split()) < 4 or any(l["text"].lower() == text.lower() for l in self.data["lessons"]):
            return False
        self.data["lessons"].append({"text": text, "source": source, "ts": int(time.time())})
        self.data["lessons"] = self.data["lessons"][-MAX_LESSONS:]
        self._save()
        return True

    def forget(self, index: int) -> str | None:
        ls = self.data["lessons"]
        if 1 <= index <= len(ls):
            gone = ls.pop(index - 1)
            self._save()
            return gone["text"]
        return None

    def lessons(self) -> list[str]:
        return [l["text"] for l in self.data["lessons"]]

    def prompt_section(self) -> str:
        out, used = [], 0
        for text in reversed(self.lessons()):  # newest first, so old ones fall off the cap
            if used + len(text) > PROMPT_CHARS:
                break
            out.append(f"- {text}")
            used += len(text)
        if not out:
            return ""
        return "# Learned in this repo\n\nThe user taught you these; follow them:\n" + "\n".join(out)

    # edit history ---------------------------------------------------------------
    def record_run(self, request: str, changed: list[str]) -> None:
        """Call only for runs that ended verified; `changed` are repo-relative paths."""
        terms = sorted(set(tokenize(request)))[:40]
        files = [f for f in changed if (self.root / f).is_file()][:8]
        if len(terms) < 3 or not files:
            return
        self.data["edits"].append({"terms": terms, "files": files, "ts": int(time.time())})
        self.data["edits"] = self.data["edits"][-MAX_EDITS:]
        self._save()

    def prior(self, query: str) -> dict[str, float]:
        """File -> boost (0..1) from past verified runs whose request resembled this query."""
        q = set(tokenize(query))
        if not q:
            return {}
        boosts: dict[str, float] = {}
        for e in self.data["edits"]:
            t = set(e["terms"])
            overlap = len(q & t) / len(q | t)
            if overlap >= MIN_OVERLAP:
                for f in e["files"]:
                    boosts[f] = min(1.0, boosts.get(f, 0.0) + overlap)
        return boosts
