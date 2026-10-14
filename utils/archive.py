"""Full copies of tool results that left the model's context.

When a result is cleared by pruning or cut by the output cap, the model keeps a
short note with an id (like `r7`). The whole text stays here, so `recall_result`
can hand back the exact original instead of the model guessing or re-running a
command that may not give the same answer twice."""
from __future__ import annotations

import hashlib
from pathlib import Path


class ResultStore:
    def __init__(self, directory: Path | None = None):
        self._mem: dict[str, str] = {}
        self._dir = directory
        self._n = 0

    def put(self, text: str) -> str:
        """Keep `text`, return its id. Saving to disk is best effort (resume, audit); memory always works."""
        self._n += 1
        rid = f"r{self._n}"
        self._mem[rid] = text
        if self._dir is not None:
            try:
                self._dir.mkdir(parents=True, exist_ok=True)
                (self._dir / f"{rid}.txt").write_text(text, encoding="utf-8")
            except OSError:
                pass
        return rid

    def get(self, rid: str) -> str | None:
        if rid in self._mem:
            return self._mem[rid]
        if self._dir is not None and rid.isalnum():
            try:
                return (self._dir / f"{rid}.txt").read_text(encoding="utf-8")
            except OSError:
                return None
        return None


def digest(text: str) -> str:
    return hashlib.sha1(text.encode("utf-8", "replace")).hexdigest()[:10]
