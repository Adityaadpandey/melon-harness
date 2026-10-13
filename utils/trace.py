"""Structured event log. One JSON object per line, one file per session.

Everything the harness decides (model picked, fallback, retry, tool run) goes
through here so a bad run can be replayed from the file. Secrets are scrubbed
on the way in; values the caller marks as long are clipped."""
from __future__ import annotations

import json
import os
import time
from pathlib import Path
from typing import Any


class Tracer:
    def __init__(self, session_id: str, directory: Path | None = None):
        self.session_id = session_id
        self.events: list[dict[str, Any]] = []
        self.path: Path | None = None
        if directory is not None:
            try:
                directory.mkdir(parents=True, exist_ok=True)
                self.path = directory / f"{session_id}.jsonl"
            except OSError:
                self.path = None  # tracing must never break a run

    @staticmethod
    def _scrub(value: Any) -> Any:
        secret = os.environ.get("API_KEY")
        if isinstance(value, str):
            if secret and secret in value:
                value = value.replace(secret, "***")
            return value if len(value) <= 2000 else value[:2000] + "...[clipped]"
        if isinstance(value, dict):
            return {k: Tracer._scrub(v) for k, v in value.items()}
        if isinstance(value, (list, tuple)):
            return [Tracer._scrub(v) for v in value]
        return value

    def emit(self, event: str, **fields: Any) -> None:
        rec = {"t": round(time.time(), 3), "session": self.session_id, "event": event}
        rec.update(self._scrub(fields))
        self.events.append(rec)
        if self.path:
            try:
                with self.path.open("a", encoding="utf-8") as f:
                    f.write(json.dumps(rec, default=str) + "\n")
            except OSError:
                pass

    def count(self, event: str) -> int:
        return sum(1 for e in self.events if e["event"] == event)
