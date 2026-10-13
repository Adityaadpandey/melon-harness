"""Scripted stand-in for the LLM client so loop behaviour is testable offline."""
from client.response import (
    StreamEvent,
    StreamEventType,
    TextDelta,
    TokenUsage,
    ToolCall,
)


def text(s: str):
    return [
        StreamEvent(type=StreamEventType.TEXT_DELTA, text_delta=TextDelta(s)),
        StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, usage=TokenUsage(1, 1, 2)),
    ]


def call(name: str, args: dict, call_id: str = "c1"):
    return [
        StreamEvent(
            type=StreamEventType.TOOL_CALL_COMPLETE,
            tool_call=ToolCall(call_id=call_id, name=name, arguments=args),
        ),
        StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, usage=TokenUsage(1, 1, 2)),
    ]


def calls(*pairs):
    evs = [
        StreamEvent(
            type=StreamEventType.TOOL_CALL_COMPLETE,
            tool_call=ToolCall(call_id=f"c{i}", name=n, arguments=a),
        )
        for i, (n, a) in enumerate(pairs)
    ]
    return evs + [StreamEvent(type=StreamEventType.MESSAGE_COMPLETE)]


def error(msg: str):
    return [StreamEvent(type=StreamEventType.ERROR, error=msg)]


class ScriptedClient:
    """Each chat_completion() call replays the next scripted turn."""

    def __init__(self, turns):
        self.turns = list(turns)
        self.requests = []

    async def chat_completion(self, messages, tools=None, stream=True):
        self.requests.append(messages)
        for ev in self.turns.pop(0):
            yield ev

    async def close(self):
        pass
