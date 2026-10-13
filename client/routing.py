"""Model registry (what exists, what it supports) and router (which one to
use for a given job). Adding a model needs no code: it shows up in /models,
and a role's chain in config decides when it gets used."""
from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from pathlib import Path

import httpx

from config.config import Config

CATALOG_TTL = 24 * 3600


@dataclass
class ModelInfo:
    id: str
    context_length: int = 128_000
    endpoints: list[str] = field(default_factory=lambda: ["/chat/completions"])
    # None = never probed. Filled from `scripts/probe_models.py` output.
    callable: bool | None = None
    tools_ok: bool | None = None

    @property
    def endpoint(self) -> str:
        # /messages is the only route for Claude models; /responses is not
        # implemented, so chat/completions is the fallback for everything else
        if "/messages" in self.endpoints:
            return "/messages"
        return "/chat/completions"


def _guess(model_id: str, ctx: int) -> ModelInfo:
    eps = ["/messages"] if model_id.startswith("claude-") else ["/chat/completions"]
    return ModelInfo(model_id, ctx, eps)


class ModelRegistry:
    def __init__(self, config: Config, data_dir: Path):
        self.config = config
        self.data_dir = data_dir
        self._models: dict[str, ModelInfo] = {}
        self._cooldown: dict[str, float] = {}

    async def load(self) -> None:
        """Fetch the live catalog (cached for a day, usable offline)."""
        cache = self.data_dir / "models.json"
        data = None
        try:
            if cache.exists() and time.time() - cache.stat().st_mtime < CATALOG_TTL:
                data = json.loads(cache.read_text())
        except (OSError, ValueError):
            data = None

        if data is None and self.config.api_key:
            try:
                async with httpx.AsyncClient(timeout=10) as c:
                    r = await c.get(
                        f"{self.config.base_url}/models",
                        headers={"Authorization": f"Bearer {self.config.api_key}"},
                    )
                if r.status_code == 200:
                    data = r.json().get("data", [])
                    self.data_dir.mkdir(parents=True, exist_ok=True)
                    cache.write_text(json.dumps(data))
            except (httpx.HTTPError, ValueError, OSError):
                pass
        if data is None and cache.exists():
            try:
                data = json.loads(cache.read_text())  # stale beats nothing
            except (OSError, ValueError):
                data = None

        for m in data or []:
            self._models[m["id"]] = ModelInfo(
                id=m["id"],
                context_length=m.get("context_length") or 128_000,
                endpoints=m.get("supported_endpoints") or ["/chat/completions"],
            )
        self._apply_probe()

    def _apply_probe(self) -> None:
        path = self.data_dir / "model_probe.json"
        try:
            rows = json.loads(path.read_text())
        except (OSError, ValueError):
            return
        for r in rows:
            info = self._models.get(r["id"])
            if info:
                info.callable = r.get("ok")
                info.tools_ok = r.get("tools_ok")

    def get(self, model_id: str) -> ModelInfo:
        info = self._models.get(model_id)
        if info is None:
            # unknown to the catalog (offline, or a brand-new model): trust the
            # id and the configured window rather than refusing to run
            return _guess(model_id, self.config.model.context_window)  # not cached: keeps validate() honest
        return info

    def validate(self, model_id: str) -> tuple[bool, list[str]]:
        """(known?, close matches). With no catalog (offline) anything goes."""
        if not self._models or model_id in self._models:
            return True, []
        import difflib

        ids = list(self._models)
        near = difflib.get_close_matches(model_id, ids, n=4, cutoff=0.5)
        tail = model_id.split("/")[-1].lower()
        near += [i for i in ids if tail in i.lower() and i not in near][:4]
        return False, near[:5]

    def all(self) -> list[ModelInfo]:
        return list(self._models.values())

    def mark_down(self, model_id: str, seconds: float) -> None:
        self._cooldown[model_id] = time.monotonic() + seconds

    def is_cooling(self, model_id: str) -> bool:
        return self._cooldown.get(model_id, 0) > time.monotonic()


class Router:
    def __init__(self, config: Config, registry: ModelRegistry):
        self.config = config
        self.registry = registry

    def chain(self, role: str) -> list[str]:
        """Configured preference order for a role. The main model always
        leads the default chain, so `--model x` is honoured over the config."""
        primary = self.config.model_name
        if role == "default":
            extra = self.config.routing.get("default", [])
            return [primary] + [m for m in extra if m != primary]
        configured = self.config.routing.get(role)
        chain = list(configured) if configured else self.chain("default")
        if role == "review":
            # a reviewer that is the author's own model shares its blind spots
            chain = [m for m in chain if m != primary] + [m for m in chain if m == primary]
        return chain

    def candidates(
        self, role: str = "default", *, need_tools: bool = False, min_context: int = 0
    ) -> tuple[list[str], list[str]]:
        """Returns (usable models in order, human-readable reasons others were
        skipped). Skips are never silent: the caller surfaces them."""
        usable: list[str] = []
        skipped: list[str] = []
        for i, mid in enumerate(self.chain(role)):
            info = self.registry.get(mid)
            explicit = i == 0 and role == "default"
            if info.callable is False and not explicit:
                skipped.append(f"{mid}: not callable with this key")
            elif need_tools and info.tools_ok is False:
                skipped.append(f"{mid}: no tool calling")
            elif min_context and info.context_length < min_context:
                skipped.append(f"{mid}: context {info.context_length} < {min_context}")
            elif self.registry.is_cooling(mid):
                skipped.append(f"{mid}: cooling down after recent failure")
            else:
                usable.append(mid)
        return usable, skipped
