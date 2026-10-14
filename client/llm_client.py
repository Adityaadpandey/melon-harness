from __future__ import annotations

import asyncio
import json
import random
import time
from pathlib import Path
from typing import Any, AsyncGenerator

from client.adapters import AnthropicAdapter, CodexResponsesAdapter, OpenAIChatAdapter
from client.errors import ProviderError
from client.response import StreamEvent, StreamEventType, TextDelta, TokenUsage, ToolCall
from client.routing import ModelRegistry, Router
from config.config import Config
from config.loader import get_data_dir
from utils.trace import Tracer


def _provider(model: str) -> str:
    return "codex" if model.startswith("codex/") else "gateway"


class LLMClient:
    """One entry point for every model call. Picks candidates through the
    router, talks to the right wire protocol, retries transient failures with
    jittered backoff, and falls back to the next candidate when a model is
    unusable. Fallbacks are announced as NOTICE events, never silent."""

    def __init__(self, config: Config, tracer: Tracer | None = None) -> None:
        self.config = config
        self.tracer = tracer or Tracer("detached")
        self._max_retries = 3
        self._adapters: dict[str, Any] = {}
        self._no_temperature: set[str] = set()
        self.registry = ModelRegistry(config, get_data_dir())
        self.router = Router(config, self.registry)
        self.last_model: str | None = None
        self._quota_blocked: set[str] = set()  # providers out of quota this session

    async def initialize(self) -> None:
        await self.registry.load()

    def _adapter(self, endpoint: str):
        if endpoint not in self._adapters:
            if endpoint == "/responses":
                self._adapters[endpoint] = CodexResponsesAdapter()
                return self._adapters[endpoint]
            cls = AnthropicAdapter if endpoint == "/messages" else OpenAIChatAdapter
            headers = {"x-cmd-zdr": "1"} if self.config.zdr else None
            self._adapters[endpoint] = cls(self.config.api_key, self.config.base_url, headers)
        return self._adapters[endpoint]

    async def close(self) -> None:
        for a in self._adapters.values():
            await a.close()
        self._adapters.clear()

    @staticmethod
    def _backoff(attempt: int) -> float:
        return min(30.0, 2**attempt) * random.uniform(0.5, 1.0)

    async def chat_completion(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        stream: bool = True,
        role: str = "default",
    ) -> AsyncGenerator[StreamEvent, None]:
        est_tokens = len(json.dumps(messages, default=str)) // 3
        models, skipped = self.router.candidates(
            role, need_tools=bool(tools), min_context=est_tokens
        )
        models = [m for m in models if _provider(m) not in self._quota_blocked]
        for s in skipped:
            self.tracer.emit("model_skipped", role=role, reason=s)
        if not models:
            why = "; ".join(skipped) or "no models configured"
            yield StreamEvent(type=StreamEventType.ERROR, error=f"No usable model for role '{role}' ({why})")
            return

        last_error = "unknown error"
        queue, prev = list(models), None
        while queue:
            model = queue.pop(0)
            if prev:
                yield StreamEvent(
                    type=StreamEventType.NOTICE,
                    notice=f"Falling back from {prev} to {model} ({last_error[:120]})",
                )
            prev = model
            outcome: ProviderError | None = None
            collected = _Collector()
            async for ev in self._call_model(model, role, messages, tools, stream, collected):
                if isinstance(ev, ProviderError):
                    outcome = ev
                else:
                    yield ev
            if outcome is None:
                return
            last_error = f"{model}: {outcome}"
            if outcome.quota:
                # every model on this provider shares the quota: skip the rest of them
                self._quota_blocked.add(_provider(model))
                queue = [m for m in queue if _provider(m) not in self._quota_blocked]
            if "unsupported_model" in str(outcome) or "not supported on this endpoint" in str(outcome):
                _, near = self.registry.validate(model)
                last_error += f". Not in the model catalog" + (f"; did you mean: {', '.join(near)}?" if near else ".")
            if collected.emitted or not outcome.fallback:
                # output already reached the caller (a fallback would duplicate
                # it), or the request itself is bad: stop here
                yield StreamEvent(type=StreamEventType.ERROR, error=last_error)
                return
        yield StreamEvent(type=StreamEventType.ERROR, error=f"All candidate models failed. Last: {last_error}")

    async def _call_model(self, model, role, messages, tools, stream, collected):
        info = self.registry.get(model)
        adapter = self._adapter(info.endpoint)
        temperature = None if model in self._no_temperature else self.config.temperature

        for attempt in range(self._max_retries + 1):
            started = time.perf_counter()
            first_token: float | None = None
            text, calls, usage, finish = "", [], None, None
            try:
                async for ev in adapter.stream(model, messages, tools, temperature):
                    if first_token is None and ev.type in (
                        StreamEventType.TEXT_DELTA,
                        StreamEventType.TOOL_CALL_START,
                    ):
                        first_token = time.perf_counter() - started
                    if ev.type == StreamEventType.TEXT_DELTA:
                        text += ev.text_delta.content
                    elif ev.type == StreamEventType.TOOL_CALL_COMPLETE:
                        calls.append(ev.tool_call)
                    elif ev.type == StreamEventType.MESSAGE_COMPLETE:
                        usage, finish = ev.usage, ev.finish_reason
                        continue  # re-emitted below, in the requested shape
                    if stream:
                        collected.emitted = True
                        yield ev
                self.last_model = model
                self.tracer.emit(
                    "model_call", model=model, role=role, attempt=attempt, ok=True,
                    sec=round(time.perf_counter() - started, 3),
                    ttft=round(first_token, 3) if first_token is not None else None,
                    tokens=usage.__dict__ if usage else None, tool_calls=len(calls),
                    request_chars=request_chars(messages),
                )
                if stream:
                    yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, finish_reason=finish, usage=usage)
                else:
                    for c in calls:
                        yield StreamEvent(type=StreamEventType.TOOL_CALL_COMPLETE, tool_call=c)
                    yield StreamEvent(
                        type=StreamEventType.MESSAGE_COMPLETE,
                        text_delta=TextDelta(text) if text else None,
                        finish_reason=finish, usage=usage,
                    )
                return
            except ProviderError as e:
                self.tracer.emit(
                    "model_call", model=model, role=role, attempt=attempt, ok=False,
                    status=e.status, error=str(e)[:300],
                    sec=round(time.perf_counter() - started, 3),
                )
                if e.drop_temperature and temperature is not None and not collected.emitted:
                    self._no_temperature.add(model)
                    temperature = None
                    continue
                if e.retry and attempt < self._max_retries and not collected.emitted:
                    await asyncio.sleep(self._backoff(attempt))
                    continue
                if e.fallback:
                    self.registry.mark_down(model, 3600 if e.status in (403, 404) else 60)
                yield e
                return


def request_chars(messages: list) -> dict[str, int]:
    """Size of the request by part (characters, so an estimate): shows what the prompt is made of without storing any of it."""
    out = {"system": 0, "user": 0, "assistant": 0, "tool_calls": 0, "tool_results": 0}
    for m in messages:
        role, text = m.get("role"), m.get("content") or ""
        if role == "tool":
            out["tool_results"] += len(text)
        elif role in ("system", "user"):
            out[role] += len(text)
        else:
            out["assistant"] += len(text)
            out["tool_calls"] += sum(len(tc["function"]["arguments"]) + len(tc["function"]["name"]) for tc in m.get("tool_calls", []))
    return out


class _Collector:
    """Remembers whether any streamed output has already reached the caller."""

    emitted = False
