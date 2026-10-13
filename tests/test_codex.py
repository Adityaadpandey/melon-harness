import json

from client.adapters import CodexResponsesAdapter, to_responses_input
from client.response import StreamEventType
from client.routing import _guess


def test_history_conversion():
    msgs = [
        {"role": "system", "content": "be brief"},
        {"role": "user", "content": "hi"},
        {"role": "assistant", "content": "", "tool_calls": [
            {"id": "c1", "function": {"name": "read", "arguments": '{"p":1}'}}]},
        {"role": "tool", "tool_call_id": "c1", "content": "data"},
    ]
    instr, items = to_responses_input(msgs)
    assert instr == "be brief"
    assert [i["type"] for i in items] == ["message", "function_call", "function_call_output"]
    assert items[1]["call_id"] == items[2]["call_id"] == "c1"


def test_codex_models_route_to_responses():
    assert _guess("codex/gpt-5.5", 1).endpoint == "/responses"


class FakeResp:
    def __init__(self, events):
        self.lines = ["data: " + json.dumps(e) for e in events]

    async def aiter_lines(self):
        for line in self.lines:
            yield line


async def test_stream_parsing():
    events = [
        {"type": "response.output_text.delta", "delta": "hey"},
        {"type": "response.output_item.added", "item": {"type": "function_call", "id": "i1", "call_id": "c1", "name": "grep"}},
        {"type": "response.output_item.done", "item": {"type": "function_call", "id": "i1", "call_id": "c1", "name": "grep", "arguments": '{"q":"x"}'}},
        {"type": "response.completed", "response": {"usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}},
    ]
    out = [e async for e in CodexResponsesAdapter(auth=object())._consume(FakeResp(events))]
    kinds = [e.type for e in out]
    assert StreamEventType.TEXT_DELTA in kinds and StreamEventType.TOOL_CALL_COMPLETE in kinds
    assert out[-1].finish_reason == "tool_calls" and out[-1].usage.total_tokens == 12


def test_codex_requests_carry_a_stable_cache_session():
    class Auth:
        def credentials(self, refresh=False):
            return "tok", "acct"

    a = CodexResponsesAdapter(auth=Auth())
    h1, h2 = a._headers(), a._headers()
    # without this header the backend reports 0 cached tokens on every turn
    assert h1["session_id"] == h2["session_id"] == a._cache_key and len(a._cache_key) >= 32
    assert CodexResponsesAdapter(auth=Auth())._cache_key != a._cache_key


def test_codex_luna_is_the_default_model():
    from config.config import Config

    assert Config().model_name == "codex/gpt-5.6-luna"
