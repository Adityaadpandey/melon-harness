from __future__ import annotations
from dataclasses import dataclass
from enum import Enum
from typing import Any
import json
import re


@dataclass
class TextDelta:
    content: str

    def __str__(self):
        return self.content


class StreamEventType(str, Enum):
    TEXT_DELTA = "text_delta"
    MESSAGE_COMPLETE = "message_complete"
    ERROR = "error"
    NOTICE = "notice"

    TOOL_CALL_START = "tool_call_start"
    TOOL_CALL_DELTA = "tool_call_delta"
    TOOL_CALL_COMPLETE = "tool_call_complete"


@dataclass
class TokenUsage:
    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0
    cached_tokens: int = 0
    cache_write_tokens: int = 0  # tokens the provider stored for later reuse (billed at a premium by some)

    def __add__(self, other: TokenUsage):
        return TokenUsage(
            prompt_tokens=self.prompt_tokens + other.prompt_tokens,
            completion_tokens=self.completion_tokens + other.completion_tokens,
            total_tokens=self.total_tokens + other.total_tokens,
            cached_tokens=self.cached_tokens + other.cached_tokens,
            cache_write_tokens=self.cache_write_tokens + other.cache_write_tokens,
        )


@dataclass
class ToolCallDelta:
    call_id: str
    name: str | None = None
    arguments_delta: str = ""


@dataclass
class ToolCall:
    call_id: str
    name: str | None = None
    arguments: str = ""


@dataclass
class StreamEvent:
    type: StreamEventType
    text_delta: TextDelta | None = None
    error: str | None = None
    notice: str | None = None
    finish_reason: str | None = None
    tool_call_delta: ToolCallDelta | None = None
    tool_call: ToolCall | None = None
    usage: TokenUsage | None = None
    reasoning: str | None = None  # the model's thinking text, on MESSAGE_COMPLETE, for models that must get it back


@dataclass
class ToolResultMessage:
    tool_call_id: str
    content: str
    is_error: bool = False

    def to_openai_message(self) -> dict[str, Any]:
        return {
            "role": "tool",
            "tool_call_id": self.tool_call_id,
            "content": self.content,
        }


def parse_tool_call_arguments(arguments_str: str) -> dict[str, Any]:
    """The model's argument string as a dict. Open models often send almost-JSON, so a few safe repairs run before
    giving up: raw newlines or tabs inside strings, a regex backslash left unescaped (a pattern like \\d+ sent with one backslash), a ```json fence,
    or the whole object encoded twice. Anything else comes back as raw_arguments and the model is asked to re-send."""
    if not arguments_str or not arguments_str.strip():
        return {}
    text = arguments_str.strip()
    if text.startswith("```"):
        text = re.sub(r"^```\w*\s*|\s*```$", "", text)
    # escape pairs are read left to right: a proper double backslash stays, a lone one before a letter is doubled
    lone = re.sub(r"\\(.)", lambda m: m.group(0) if m.group(1) in '"\\/bfnrtu' else "\\\\" + m.group(1), text, flags=re.S)
    for attempt in (text, lone):
        try:
            value = json.loads(attempt, strict=False)
        except json.JSONDecodeError:
            continue
        if isinstance(value, str):  # '"{\"path\": \"a\"}"': the object was serialised twice
            try:
                value = json.loads(value, strict=False)
            except json.JSONDecodeError:
                pass
        if isinstance(value, dict):
            return value
    return {"raw_arguments": arguments_str}
