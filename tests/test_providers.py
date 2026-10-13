import json
from types import SimpleNamespace as NS

import pytest

from client.adapters import OpenAIChatAdapter, to_anthropic_messages
from client.errors import ProviderError, classify
from client.llm_client import LLMClient
from client.response import StreamEvent, StreamEventType, TextDelta
from client.routing import ModelInfo
from config.config import Config


def test_anthropic_conversion_merges_tool_results_and_patches_orphans():
    msgs = [
        {"role": "system", "content": "sys"},
        {"role": "user", "content": "do it"},
        {
            "role": "assistant",
            "content": "",
            "tool_calls": [
                {"id": "a", "type": "function", "function": {"name": "x", "arguments": '{"k": 1}'}},
                {"id": "b", "type": "function", "function": {"name": "y", "arguments": "{}"}},
            ],
        },
        {"role": "tool", "tool_call_id": "a", "content": "res-a"},
        # result for "b" is missing (pruned / crashed mid-turn)
        {"role": "user", "content": "carry on"},
    ]
    system, out = to_anthropic_messages(msgs)
    assert system == [{"type": "text", "text": "sys"}]
    assert [m["role"] for m in out] == ["user", "assistant", "user"]
    assert out[1]["content"][0]["input"] == {"k": 1}
    blocks = out[2]["content"]
    assert [b["type"] for b in blocks] == ["tool_result", "tool_result", "text"]
    assert blocks[1]["tool_use_id"] == "b" and blocks[1]["is_error"]


def chunk(content=None, tcs=None, finish=None, usage=None):
    return NS(
        usage=usage,
        choices=[NS(delta=NS(content=content, tool_calls=tcs), finish_reason=finish)],
    )


def tc(index, id=None, name=None, args=None):
    return NS(index=index, id=id, function=NS(name=name, arguments=args))


async def test_openai_stream_accumulates_arguments_across_chunks():
    """Regression: arguments after the first chunk used to be dropped."""
    async def gen():
        yield chunk(tcs=[tc(0, "c1", "write_file", '{"pa')])
        yield chunk(tcs=[tc(0, None, None, 'th": "a.txt", ')])
        yield chunk(tcs=[tc(0, None, None, '"content": "hi"}')])
        yield chunk(tcs=[tc(1, "c2", "read_file", '{"path": "b"}')], finish="tool_calls")

    ad = OpenAIChatAdapter("k", "http://x")
    evs = [e async for e in ad._consume(gen())]
    done = [e.tool_call for e in evs if e.type == StreamEventType.TOOL_CALL_COMPLETE]
    assert [(c.call_id, c.name, c.arguments) for c in done] == [
        ("c1", "write_file", {"path": "a.txt", "content": "hi"}),
        ("c2", "read_file", {"path": "b"}),
    ]


@pytest.mark.parametrize(
    "status,msg,retry,fallback",
    [
        (429, "slow down", True, True),
        (503, "oops", True, True),
        (None, "conn reset", True, True),
        (403, "MODEL_NOT_IN_PLAN: x", False, True),
        (400, "No available providers match", False, True),
        (404, "model not found", False, True),
        (401, "bad key", False, False),
        (400, "invalid tool schema", False, False),
    ],
)
def test_error_classification(status, msg, retry, fallback):
    e = classify(status, msg)
    assert (e.retry, e.fallback) == (retry, fallback)


def test_temperature_rejection_is_recognised():
    assert classify(400, "temperature is not supported").drop_temperature


class FakeAdapter:
    def __init__(self, script):
        self.script = script  # model -> list of (ProviderError | text)
        self.calls = []

    async def stream(self, model, messages, tools, temperature):
        self.calls.append((model, temperature))
        step = self.script[model].pop(0)
        if isinstance(step, ProviderError):
            raise step
        yield StreamEvent(type=StreamEventType.TEXT_DELTA, text_delta=TextDelta(step))
        yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE)

    async def close(self):
        pass


def make_client(tmp_path, script, routing=None, monkeypatch=None):
    cfg = Config(cwd=tmp_path, routing=routing or {})
    cfg.model_name = "m1"
    c = LLMClient(cfg)
    c._max_retries = 2
    c._backoff = lambda attempt: 0
    fake = FakeAdapter(script)
    c._adapter = lambda endpoint: fake
    for mid in ("m1", "m2", "m3"):
        c.registry._models[mid] = ModelInfo(mid, 1_000_000)
    return c, fake


async def collect(c, **kw):
    return [e async for e in c.chat_completion([{"role": "user", "content": "hi"}], **kw)]


async def test_transient_error_is_retried_on_same_model(tmp_path):
    c, fake = make_client(tmp_path, {"m1": [classify(503, "x"), "ok"]})
    evs = await collect(c)
    assert [m for m, _ in fake.calls] == ["m1", "m1"]
    assert any(e.type == StreamEventType.TEXT_DELTA for e in evs)


async def test_not_in_plan_falls_back_with_notice_and_cools_model(tmp_path):
    c, fake = make_client(
        tmp_path,
        {"m1": [classify(403, "MODEL_NOT_IN_PLAN")], "m2": ["from m2"]},
        routing={"default": ["m2"]},
    )
    evs = await collect(c)
    notices = [e.notice for e in evs if e.type == StreamEventType.NOTICE]
    assert len(notices) == 1 and "m1" in notices[0] and "m2" in notices[0]
    assert c.registry.is_cooling("m1")
    # next request skips m1 up front instead of paying for another 403
    c.router.candidates("default")[0] == ["m2"]


async def test_permanent_error_does_not_fall_back(tmp_path):
    c, fake = make_client(
        tmp_path, {"m1": [classify(400, "bad schema")], "m2": ["x"]}, routing={"default": ["m2"]}
    )
    evs = await collect(c)
    assert [m for m, _ in fake.calls] == ["m1"]
    assert evs[-1].type == StreamEventType.ERROR


async def test_temperature_is_dropped_once_then_remembered(tmp_path):
    c, fake = make_client(tmp_path, {"m1": [classify(400, "temperature unsupported"), "ok", "ok2"]})
    await collect(c)
    await collect(c)
    assert [t is None for _, t in fake.calls] == [False, True, True]


async def test_router_skips_models_known_uncallable_or_too_small(tmp_path):
    c, _ = make_client(tmp_path, {}, routing={"review": ["m2", "m3"]})
    c.registry._models["m2"].callable = False
    c.registry._models["m3"].context_length = 10
    usable, skipped = c.router.candidates("review", min_context=1000)
    assert usable == [] and len(skipped) == 2


async def test_explicit_main_model_is_not_skipped_by_probe(tmp_path):
    c, _ = make_client(tmp_path, {})
    c.registry._models["m1"].callable = False
    assert c.router.candidates("default")[0] == ["m1"]


def test_model_validation_suggests_close_matches(tmp_path):
    c, _ = make_client(tmp_path, {})
    c.registry._models.clear()
    for mid in ("gpt-6-luna", "gpt-5.6-luna", "Qwen/Qwen3.8-Max"):
        c.registry._models[mid] = ModelInfo(mid)
    ok, near = c.registry.validate("openai/gpt-luna-6")
    assert not ok and "gpt-6-luna" in near
    assert c.registry.validate("Qwen/Qwen3.8-Max") == (True, [])
    c.registry._models.clear()
    assert c.registry.validate("anything")[0]  # offline: don't block


async def test_unsupported_model_error_suggests_real_ids(tmp_path):
    c, fake = make_client(tmp_path, {"gpt-luna-6": [classify(400, "Model \"m1\" is not supported on this endpoint. code: unsupported_model")]})
    c.registry._models.clear()
    c.registry._models["gpt-6-luna"] = ModelInfo("gpt-6-luna")
    c.config.model_name = "gpt-luna-6"
    evs = await collect(c)
    assert "did you mean: gpt-6-luna" in evs[-1].error


def test_plan_wide_usage_limit_is_not_retried_or_failed_over():
    e = classify(429, "You've reached your 5-hour usage limit for your plan. Your limit resets at 19:12")
    assert not e.retry and not e.fallback
    assert classify(429, "slow down").retry  # ordinary burst limits still retry
