"""Wire-format adapters. Each one turns our OpenAI-style message history into
one provider protocol and streams back normalized StreamEvents. Nothing in
here knows about routing, retries or fallback; that lives in LLMClient."""
from __future__ import annotations

import json
import uuid
from typing import Any, AsyncGenerator

import httpx

from client.errors import ProviderError, classify
from client.response import (
    StreamEvent,
    StreamEventType,
    TextDelta,
    TokenUsage,
    ToolCall,
    ToolCallDelta,
    parse_tool_call_arguments,
)

TIMEOUT = httpx.Timeout(120.0, connect=15.0)


def _tool_defs(tools: list[dict[str, Any]]) -> list[dict[str, Any]]:
    empty = {"type": "object", "properties": {}}
    return [
        {
            "type": "function",
            "function": {
                "name": t["name"],
                "description": t.get("description", ""),
                "parameters": t.get("parameters", empty),
            },
        }
        for t in tools
    ]


class OpenAIChatAdapter:
    endpoint = "/chat/completions"

    def __init__(self, api_key: str, base_url: str, headers: dict[str, str] | None = None):
        from openai import AsyncOpenAI

        self._client = AsyncOpenAI(
            api_key=api_key, base_url=base_url, timeout=TIMEOUT, max_retries=0,
            default_headers=headers,
        )

    async def close(self) -> None:
        await self._client.close()

    async def stream(
        self,
        model: str,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None,
        temperature: float | None,
    ) -> AsyncGenerator[StreamEvent, None]:
        kwargs: dict[str, Any] = {
            "model": model,
            "messages": messages,
            "stream": True,
            "stream_options": {"include_usage": True},
        }
        if temperature is not None:
            kwargs["temperature"] = temperature
        if tools:
            kwargs["tools"] = _tool_defs(tools)
            kwargs["tool_choice"] = "auto"

        try:
            response = await self._client.chat.completions.create(**kwargs)
            async for ev in self._consume(response):
                yield ev
        except Exception as e:
            status = getattr(e, "status_code", None)
            if status is not None:
                raise classify(status, str(e)) from e
            if e.__class__.__name__ == "APIConnectionError" or isinstance(e, httpx.HTTPError):
                raise classify(None, f"Connection error: {e}") from e
            raise

    async def _consume(self, response) -> AsyncGenerator[StreamEvent, None]:
        finish_reason: str | None = None
        usage: TokenUsage | None = None
        calls: dict[int, dict[str, str]] = {}

        async for chunk in response:
            if getattr(chunk, "usage", None):
                u = chunk.usage
                details = getattr(u, "prompt_tokens_details", None)
                usage = TokenUsage(
                    prompt_tokens=u.prompt_tokens or 0,
                    completion_tokens=u.completion_tokens or 0,
                    total_tokens=u.total_tokens or 0,
                    cached_tokens=(getattr(details, "cached_tokens", 0) or 0),
                )
            if not chunk.choices:
                continue
            choice = chunk.choices[0]
            delta = choice.delta
            if choice.finish_reason:
                finish_reason = choice.finish_reason
            if delta.content:
                yield StreamEvent(
                    type=StreamEventType.TEXT_DELTA, text_delta=TextDelta(delta.content)
                )
            for d in delta.tool_calls or []:
                call = calls.setdefault(d.index, {"id": "", "name": "", "arguments": ""})
                if d.id:
                    call["id"] = d.id
                fn = d.function
                if fn and fn.name and not call["name"]:
                    call["name"] = fn.name
                    yield StreamEvent(
                        type=StreamEventType.TOOL_CALL_START,
                        tool_call_delta=ToolCallDelta(call_id=call["id"], name=fn.name),
                    )
                if fn and fn.arguments:
                    call["arguments"] += fn.arguments
                    yield StreamEvent(
                        type=StreamEventType.TOOL_CALL_DELTA,
                        tool_call_delta=ToolCallDelta(
                            call_id=call["id"], arguments_delta=fn.arguments
                        ),
                    )

        for idx in sorted(calls):
            c = calls[idx]
            yield StreamEvent(
                type=StreamEventType.TOOL_CALL_COMPLETE,
                tool_call=ToolCall(
                    call_id=c["id"] or f"call_{idx}",
                    name=c["name"],
                    arguments=parse_tool_call_arguments(c["arguments"]),
                ),
            )
        yield StreamEvent(
            type=StreamEventType.MESSAGE_COMPLETE,
            finish_reason=finish_reason,
            usage=usage,
        )


def to_anthropic_messages(
    messages: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]] | None, list[dict[str, Any]]]:
    """OpenAI-style history -> (system blocks, anthropic messages).

    Anthropic wants tool results as blocks inside the *next user* message, and
    rejects any tool_use that has no matching tool_result. Histories that were
    pruned, compacted or resumed can contain such orphans, so we patch them
    with a placeholder rather than failing the whole request."""
    system = [
        {"type": "text", "text": m["content"]}
        for m in messages
        if m["role"] == "system" and m.get("content")
    ]
    out: list[dict[str, Any]] = []
    pending: list[str] = []  # tool_use ids still waiting for a result

    def push_user_blocks(blocks: list[dict[str, Any]]) -> None:
        if out and out[-1]["role"] == "user":
            out[-1]["content"].extend(blocks)
        else:
            out.append({"role": "user", "content": blocks})

    def settle_pending() -> None:
        if pending:
            push_user_blocks(
                [
                    {
                        "type": "tool_result",
                        "tool_use_id": i,
                        "content": "[no result recorded]",
                        "is_error": True,
                    }
                    for i in pending
                ]
            )
            pending.clear()

    for m in messages:
        role = m["role"]
        if role == "system":
            continue
        if role == "tool":
            tid = m["tool_call_id"]
            if tid in pending:
                pending.remove(tid)
            push_user_blocks(
                [
                    {
                        "type": "tool_result",
                        "tool_use_id": tid,
                        "content": m.get("content") or "(no output)",
                    }
                ]
            )
        elif role == "assistant":
            settle_pending()
            blocks: list[dict[str, Any]] = []
            if m.get("content"):
                blocks.append({"type": "text", "text": m["content"]})
            for tc in m.get("tool_calls") or []:
                args = tc["function"]["arguments"]
                try:
                    parsed = json.loads(args) if isinstance(args, str) else args
                except json.JSONDecodeError:
                    parsed = {}
                blocks.append(
                    {
                        "type": "tool_use",
                        "id": tc["id"],
                        "name": tc["function"]["name"],
                        "input": parsed if isinstance(parsed, dict) else {},
                    }
                )
                pending.append(tc["id"])
            if blocks:
                out.append({"role": "assistant", "content": blocks})
        else:  # user
            settle_pending()
            push_user_blocks([{"type": "text", "text": m.get("content") or "(empty)"}])
    return system or None, out


class AnthropicAdapter:
    endpoint = "/messages"
    MAX_TOKENS = 16384

    def __init__(self, api_key: str, base_url: str, headers: dict[str, str] | None = None):
        from anthropic import AsyncAnthropic

        # the SDK appends /v1/messages itself, so strip the trailing /v1
        root = base_url.rstrip("/")
        if root.endswith("/v1"):
            root = root[:-3]
        self._client = AsyncAnthropic(
            # plain seconds: this SDK uses httpx2, so an httpx.Timeout is rejected
            api_key=api_key, base_url=root, timeout=120.0, max_retries=0,
            default_headers=headers,
        )

    async def close(self) -> None:
        await self._client.close()

    async def stream(
        self,
        model: str,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None,
        temperature: float | None,
    ) -> AsyncGenerator[StreamEvent, None]:
        system, msgs = to_anthropic_messages(messages)
        kwargs: dict[str, Any] = {
            "model": model,
            "messages": msgs,
            "max_tokens": self.MAX_TOKENS,
        }
        if msgs and msgs[-1]["content"]:
            # second breakpoint: everything up to the newest message is a stable
            # prefix on the next turn, so history is re-read at the cached rate
            msgs[-1]["content"][-1]["cache_control"] = {"type": "ephemeral"}
        if system:
            # cache the stable prefix; agent loops resend it every turn
            system[-1]["cache_control"] = {"type": "ephemeral"}
            kwargs["system"] = system
        if tools:
            kwargs["tools"] = [
                {
                    "name": t["name"],
                    "description": t.get("description", ""),
                    "input_schema": t.get(
                        "parameters", {"type": "object", "properties": {}}
                    ),
                }
                for t in tools
            ]
        # no temperature: the current Claude models reject sampling params

        try:
            async with self._client.messages.stream(**kwargs) as s:
                async for ev in s:
                    if ev.type == "text":
                        yield StreamEvent(
                            type=StreamEventType.TEXT_DELTA,
                            text_delta=TextDelta(ev.text),
                        )
                    elif (
                        ev.type == "content_block_start"
                        and ev.content_block.type == "tool_use"
                    ):
                        yield StreamEvent(
                            type=StreamEventType.TOOL_CALL_START,
                            tool_call_delta=ToolCallDelta(
                                call_id=ev.content_block.id, name=ev.content_block.name
                            ),
                        )
                final = await s.get_final_message()
        except Exception as e:
            status = getattr(e, "status_code", None)
            if status is not None:
                raise classify(status, str(e)) from e
            if e.__class__.__name__ == "APIConnectionError" or isinstance(e, httpx.HTTPError):
                raise classify(None, f"Connection error: {e}") from e
            raise

        for block in final.content:
            if block.type == "tool_use":
                yield StreamEvent(
                    type=StreamEventType.TOOL_CALL_COMPLETE,
                    tool_call=ToolCall(
                        call_id=block.id,
                        name=block.name,
                        arguments=block.input if isinstance(block.input, dict) else {},
                    ),
                )
        u = final.usage
        cached = getattr(u, "cache_read_input_tokens", 0) or 0
        created = getattr(u, "cache_creation_input_tokens", 0) or 0
        prompt = (u.input_tokens or 0) + cached + created
        yield StreamEvent(
            type=StreamEventType.MESSAGE_COMPLETE,
            finish_reason={"tool_use": "tool_calls", "end_turn": "stop"}.get(
                final.stop_reason, final.stop_reason
            ),
            usage=TokenUsage(
                prompt_tokens=prompt,
                completion_tokens=u.output_tokens or 0,
                total_tokens=prompt + (u.output_tokens or 0),
                cached_tokens=cached,
                cache_write_tokens=created,
            ),
        )


def to_responses_input(messages: list[dict[str, Any]]) -> tuple[str, list[dict[str, Any]]]:
    """OpenAI chat history -> (instructions, Responses API input items).
    System messages become `instructions`; tool calls and their results become
    function_call / function_call_output items linked by call_id."""
    instructions = "\n\n".join(m["content"] for m in messages if m["role"] == "system" and m.get("content"))
    items: list[dict[str, Any]] = []
    for m in messages:
        role = m["role"]
        if role == "system":
            continue
        if role == "tool":
            items.append({
                "type": "function_call_output",
                "call_id": m["tool_call_id"],
                "output": m.get("content") or "(no output)",
            })
        elif role == "assistant":
            if m.get("content"):
                items.append({
                    "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": m["content"]}],
                })
            for tc in m.get("tool_calls") or []:
                args = tc["function"]["arguments"]
                items.append({
                    "type": "function_call",
                    "call_id": tc["id"],
                    "name": tc["function"]["name"],
                    "arguments": args if isinstance(args, str) else json.dumps(args),
                })
        else:
            items.append({
                "type": "message", "role": "user",
                "content": [{"type": "input_text", "text": m.get("content") or "(empty)"}],
            })
    return instructions or "You are a helpful coding agent.", items


class CodexResponsesAdapter:
    """Talks to the ChatGPT-backed Codex endpoint (Responses API over SSE),
    authenticated with the stored Codex login rather than an API key."""

    endpoint = "/responses"
    URL = "https://chatgpt.com/backend-api/codex/responses"

    def __init__(self, auth=None):
        from client.codex_auth import CodexAuth

        self._auth = auth or CodexAuth()
        self._http = httpx.AsyncClient(timeout=TIMEOUT)
        self._cache_key = str(uuid.uuid4())  # stable per session: keeps every turn on the backend that holds our prompt cache

    async def close(self) -> None:
        await self._http.aclose()

    def _headers(self, force_refresh: bool = False) -> dict[str, str]:
        token, account = self._auth.credentials(force_refresh)
        return {
            "Authorization": f"Bearer {token}",
            "chatgpt-account-id": account,
            "OpenAI-Beta": "responses=experimental",
            "originator": "codex_cli_rs",
            "session_id": self._cache_key,  # the Codex CLI sends its conversation id here and as prompt_cache_key
            "Accept": "text/event-stream",
        }

    async def stream(
        self,
        model: str,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None,
        temperature: float | None,
    ) -> AsyncGenerator[StreamEvent, None]:
        # the "codex/" prefix only routes inside mew; the backend wants the bare name
        instructions, items = to_responses_input(messages)
        body: dict[str, Any] = {
            "model": model.removeprefix("codex/"),
            "instructions": instructions,
            "input": items,
            "store": False,
            "stream": True,
            "prompt_cache_key": self._cache_key,
        }
        if tools:
            body["tools"] = [
                {
                    "type": "function",
                    "name": t["name"],
                    "description": t.get("description", ""),
                    "parameters": t.get("parameters", {"type": "object", "properties": {}}),
                }
                for t in tools
            ]
            body["tool_choice"] = "auto"
        # no temperature: the Codex backend rejects sampling params

        try:
            headers = self._headers()
        except RuntimeError as e:
            raise ProviderError(str(e), status=401) from e
        try:
            for attempt in range(2):
                async with self._http.stream("POST", self.URL, json=body, headers=headers) as r:
                    if r.status_code == 401 and attempt == 0:
                        # token expired mid-session: refresh once and replay
                        try:
                            headers = self._headers(force_refresh=True)
                        except RuntimeError as e:
                            raise ProviderError(str(e), status=401) from e
                        continue
                    if r.status_code != 200:
                        raise classify(r.status_code, (await r.aread()).decode(errors="replace")[:500])
                    async for ev in self._consume(r):
                        yield ev
                    return
        except httpx.HTTPError as e:
            raise classify(None, f"Connection error: {e}") from e

    async def _consume(self, r) -> AsyncGenerator[StreamEvent, None]:
        usage: TokenUsage | None = None
        finish = "stop"
        names: dict[str, str] = {}  # item id -> call_id, to route argument deltas
        async for line in r.aiter_lines():
            if not line.startswith("data:"):
                continue
            payload = line[5:].strip()
            if not payload or payload == "[DONE]":
                continue
            try:
                ev = json.loads(payload)
            except ValueError:
                continue
            kind = ev.get("type", "")
            if kind == "response.output_text.delta":
                yield StreamEvent(type=StreamEventType.TEXT_DELTA, text_delta=TextDelta(ev.get("delta", "")))
            elif kind == "response.output_item.added" and ev["item"].get("type") == "function_call":
                it = ev["item"]
                names[it["id"]] = it["call_id"]
                yield StreamEvent(
                    type=StreamEventType.TOOL_CALL_START,
                    tool_call_delta=ToolCallDelta(call_id=it["call_id"], name=it["name"]),
                )
            elif kind == "response.function_call_arguments.delta":
                yield StreamEvent(
                    type=StreamEventType.TOOL_CALL_DELTA,
                    tool_call_delta=ToolCallDelta(
                        call_id=names.get(ev.get("item_id"), ""), arguments_delta=ev.get("delta", "")
                    ),
                )
            elif kind == "response.output_item.done" and ev["item"].get("type") == "function_call":
                it = ev["item"]
                finish = "tool_calls"
                yield StreamEvent(
                    type=StreamEventType.TOOL_CALL_COMPLETE,
                    tool_call=ToolCall(
                        call_id=it["call_id"],
                        name=it["name"],
                        arguments=parse_tool_call_arguments(it.get("arguments", "")),
                    ),
                )
            elif kind == "response.completed":
                u = ev["response"].get("usage") or {}
                cached = (u.get("input_tokens_details") or {}).get("cached_tokens", 0) or 0
                usage = TokenUsage(
                    prompt_tokens=u.get("input_tokens", 0),
                    completion_tokens=u.get("output_tokens", 0),
                    total_tokens=u.get("total_tokens", 0),
                    cached_tokens=cached,
                )
            elif kind in ("response.failed", "error"):
                err = ev.get("response", {}).get("error") or ev.get("error") or ev
                raise classify(None, f"Codex error: {json.dumps(err)[:300]}")
        yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, finish_reason=finish, usage=usage)
