from __future__ import annotations

import asyncio
import json
import random
import re
import time
from pathlib import Path
from typing import Any, AsyncGenerator

from client.adapters import AnthropicAdapter, CodexResponsesAdapter, OpenAIChatAdapter
from client.errors import ProviderError, classify
from client.providers import PRESETS, split
from client.response import StreamEvent, StreamEventType, TextDelta, TokenUsage, ToolCall
from client.routing import ModelRegistry, Router
from config.config import Config
from config.loader import get_data_dir
from utils.trace import Tracer


# Sampling each open-model family was tuned for, from opencode's per-provider defaults (first match on the model id
# wins). Qwen drifts and repeats itself at 1.0; Kimi, MiniMax and Gemini want top_p 0.95 with it.
SAMPLING = [("qwen", 0.55, 1.0), ("kimi", 1.0, 0.95), ("minimax", 1.0, 0.95), ("gemini", 1.0, 0.95), ("glm", 1.0, None)]


def sampling(model: str, configured: float | None) -> tuple[float | None, float | None]:
    """(temperature, top_p) for one call: a configured temperature wins, then the family table, then the server default."""
    if configured is not None:
        return configured, None
    low = model.lower()
    return next(((t, p) for key, t, p in SAMPLING if key in low), (None, None))


def thinking_body(provider: str, model: str, effort: str | None) -> dict:
    """The switch that turns a model's thinking down for an easy request and up for a hard one, in the wording of the
    server it goes to (pi's and opencode's tables). `effort` is "low" (easy), "high" (hard) or None (leave the
    server's default alone, which is what most requests get). e.g. ("zai", "glm-5.3", "high") ->
    {"thinking": {"type": "enabled", "clear_thinking": False}}: thinking on, and kept across turns.
    The Command Code gateway ignores all of these (probed 2026-10-22), so nothing is sent there."""
    if not effort or provider in ("gateway", "codex"):
        return {}
    on, low = effort != "low", model.lower()
    if "gpt-oss" in low:
        return {"reasoning_effort": effort}
    if provider == "deepseek":
        return {"thinking": {"type": "enabled" if on else "disabled"}}
    if provider == "zai":
        return {"thinking": {"type": "enabled", "clear_thinking": False} if on else {"type": "disabled"}}
    if provider == "openrouter":
        return {"reasoning": {"effort": effort}}
    if provider == "together":
        return {"reasoning": {"enabled": on}}
    if provider not in PRESETS or provider == "lmstudio":
        # your own vLLM / SGLang / LM Studio server: the switch lives in the model's chat template
        if "deepseek" in low:
            return {"chat_template_kwargs": {"thinking": on}}
        if any(f in low for f in ("qwen", "glm")):
            return {"chat_template_kwargs": {"enable_thinking": on}}
    return {}


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
        self._max_out: dict[str, int] = {}  # model -> output cap its server accepts (0 = send none)
        self._no_thinking: set[str] = set()  # models whose server refused the thinking switch
        self.registry = ModelRegistry(config, get_data_dir())
        self.router = Router(config, self.registry)
        self.last_model: str | None = None
        self._quota_blocked: set[str] = set()  # providers out of quota this session
        self.auto_effort: str | None = None  # set per request from its difficulty; an explicit config.reasoning_effort wins

    async def initialize(self) -> None:
        await self.registry.load()

    def _provider(self, model: str) -> str:
        """Which account a model is billed to: models sharing one also share its quota."""
        direct = split(self.config, model)
        return direct[0].name if direct else "codex" if model.startswith("codex/") else "gateway"

    def _direct_adapter(self, p):
        """Straight to a provider (`groq:...`), with its own URL and key."""
        if p.name not in self._adapters:
            cls = AnthropicAdapter if p.protocol == "anthropic" else OpenAIChatAdapter
            self._adapters[p.name] = cls(p.api_key or "", p.base_url, p.headers or None)
        return self._adapters[p.name]

    def _adapter(self, endpoint: str):
        if endpoint not in self._adapters:
            if endpoint == "/responses":
                self._adapters[endpoint] = CodexResponsesAdapter(effort=self.config.reasoning_effort)
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
    def _backoff(attempt: int, status: int | None = None) -> float:
        """Seconds to wait before retrying. A busy upstream (429, 503) needs longer than a dropped connection: the free MiniMax tier
        answered 429 three times in four seconds, then the harness fell back to another model and lost the prompt cache for nothing."""
        if status in (429, 503):
            return (3.0, 8.0, 20.0)[min(attempt, 2)] * random.uniform(0.8, 1.2)
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
        models = [m for m in models if self._provider(m) not in self._quota_blocked]
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
            async for ev in self._call_model(model, role, messages, tools, stream, collected, est_tokens):
                if isinstance(ev, ProviderError):
                    outcome = ev
                else:
                    yield ev
            if outcome is None:
                return
            last_error = f"{model}: {outcome}"
            if outcome.quota:
                # every model on this provider shares the quota: skip the rest of them
                self._quota_blocked.add(self._provider(model))
                queue = [m for m in queue if self._provider(m) not in self._quota_blocked]
            if "unsupported_model" in str(outcome) or "not supported on this endpoint" in str(outcome):
                _, near = self.registry.validate(model)
                last_error += f". Not in the model catalog" + (f"; did you mean: {', '.join(near)}?" if near else ".")
            if collected.emitted or not outcome.fallback:
                # output already reached the caller (a fallback would duplicate
                # it), or the request itself is bad: stop here
                yield StreamEvent(type=StreamEventType.ERROR, error=last_error)
                return
        yield StreamEvent(type=StreamEventType.ERROR, error=f"All candidate models failed. Last: {last_error}")

    def _output_cap(self, model: str, window: int, est_tokens: int) -> int | None:
        """max_tokens for this call: the configured cap, what the server said it accepts, and the room left in the
        context, whichever is smallest. e.g. a 128k window with 110k of prompt leaves 128k-110k-4k = 14k to write."""
        cap = min(self._max_out.get(model, self.config.model.max_output_tokens), max(1024, window - est_tokens - 4096))
        return cap or None

    async def _call_model(self, model, role, messages, tools, stream, collected, est_tokens=0):
        info = self.registry.get(model)
        direct = split(self.config, model)
        if direct and not direct[0].api_key:  # say which env var is missing instead of a bare 401 from the provider
            yield ProviderError(self.config.missing_key(model), status=401, fallback=True)
            return
        adapter = self._direct_adapter(direct[0]) if direct else self._adapter(info.endpoint)
        wire_model = direct[1] if direct else model  # the provider wants its own name, without our "groq:" prefix
        temperature, top_p = (None, None) if model in self._no_temperature else sampling(model, self.config.temperature)
        # summaries never need deep thinking; everything else follows the request (an explicit config value wins)
        effort = "low" if role == "summarize" else (self.config.reasoning_effort or self.auto_effort)
        if hasattr(adapter, "_effort"):  # Codex takes it as a setting rather than a request field
            adapter._effort = effort

        for attempt in range(self._max_retries + 1):
            started = time.perf_counter()
            first_token: float | None = None
            text, calls, usage, finish, reasoning = "", [], None, None, None
            cap = self._output_cap(model, info.context_length, est_tokens) if isinstance(adapter, OpenAIChatAdapter) else None
            body = {} if model in self._no_thinking else thinking_body(self._provider(model), wire_model, effort)
            extra = {k: v for k, v in (("max_tokens", cap), ("top_p", top_p), ("extra_body", body)) if v and isinstance(adapter, OpenAIChatAdapter)}
            try:
                async for ev in adapter.stream(wire_model, messages, tools, temperature, **extra):
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
                        usage, finish, reasoning = ev.usage, ev.finish_reason, ev.reasoning
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
                    yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, finish_reason=finish, usage=usage, reasoning=reasoning)
                else:
                    for c in calls:
                        yield StreamEvent(type=StreamEventType.TOOL_CALL_COMPLETE, tool_call=c)
                    yield StreamEvent(
                        type=StreamEventType.MESSAGE_COMPLETE,
                        text_delta=TextDelta(text) if text else None,
                        finish_reason=finish, usage=usage, reasoning=reasoning,
                    )
                return
            except Exception as e:
                if not isinstance(e, ProviderError):  # a parse slip or dropped connection mid-stream: treat it like a network error so it retries and falls back
                    e = classify(None, f"{type(e).__name__}: {e}")
                self.tracer.emit(
                    "model_call", model=model, role=role, attempt=attempt, ok=False,
                    status=e.status, error=str(e)[:300],
                    sec=round(time.perf_counter() - started, 3),
                )
                if e.drop_thinking and body and not collected.emitted:
                    self._no_thinking.add(model)  # this server has no such switch: leave thinking at its default
                    continue
                if e.drop_max_tokens and cap and not collected.emitted:
                    # "valid range of max_tokens is [1, 8192]" -> 8192; no usable number -> stop sending one
                    self._max_out[model] = max([n for n in map(int, re.findall(r"\d+", str(e))) if 1024 <= n < cap], default=0)
                    continue
                if e.drop_temperature and (temperature is not None or top_p) and not collected.emitted:
                    self._no_temperature.add(model)
                    temperature, top_p = None, None
                    continue
                if e.retry and attempt < self._max_retries and not collected.emitted:
                    yield StreamEvent(type=StreamEventType.NOTICE, notice=f"{model} didn't answer ({str(e)[:80]}), trying again ({attempt + 1}/{self._max_retries})")
                    await asyncio.sleep(self._backoff(attempt, e.status))
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
