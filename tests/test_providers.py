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


async def test_parallel_calls_without_an_index_stay_separate():
    # some open-model servers send index=None on every delta; they used to merge into one garbled call
    async def gen():
        yield chunk(tcs=[tc(None, "c1", "read_file", '{"path": ')])
        yield chunk(tcs=[tc(None, None, None, '"a"}')])
        yield chunk(tcs=[tc(None, "c2", "grep", '{"pattern": "x"}')], finish="tool_calls")

    evs = [e async for e in OpenAIChatAdapter("k", "http://x")._consume(gen())]
    done = [e.tool_call for e in evs if e.type == StreamEventType.TOOL_CALL_COMPLETE]
    assert [(c.call_id, c.name, c.arguments) for c in done] == [
        ("c1", "read_file", {"path": "a"}),
        ("c2", "grep", {"pattern": "x"}),
    ]


@pytest.mark.parametrize("usage", [
    NS(prompt_tokens=100, completion_tokens=5, total_tokens=105, prompt_tokens_details=NS(cached_tokens=80)),
    NS(prompt_tokens=100, completion_tokens=5, total_tokens=105, prompt_cache_hit_tokens=80),  # DeepSeek
    NS(prompt_tokens=100, completion_tokens=5, total_tokens=105, cached_tokens=80),  # Kimi
])
async def test_cache_hits_are_counted_on_every_provider(usage):
    async def gen():
        yield chunk(content="hi", finish="stop", usage=usage)

    evs = [e async for e in OpenAIChatAdapter("k", "http://x")._consume(gen())]
    assert evs[-1].usage.cached_tokens == 80


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
        (400, "Model is overloaded, try again later", True, True),  # odd status, transient text
        (422, "upstream timed out", True, True),
        (402, "Payment required", False, True),
        (403, "insufficient balance on this account", False, True),
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
        if isinstance(step, Exception):
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
    c._backoff = lambda attempt, status=None: 0
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


async def test_stray_exception_is_retried_and_tells_the_user(tmp_path):
    # a KeyError from a half-parsed stream used to skip retries and kill the turn
    c, fake = make_client(tmp_path, {"m1": [KeyError("item"), "ok"]})
    evs = await collect(c)
    assert [m for m, _ in fake.calls] == ["m1", "m1"]
    assert any(e.type == StreamEventType.NOTICE and "trying again" in e.notice for e in evs)
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
    c.config.temperature = 0.7
    await collect(c)
    await collect(c)
    assert [t is None for _, t in fake.calls] == [False, True, True]


async def test_router_skips_models_known_uncallable_or_too_small(tmp_path):
    c, _ = make_client(tmp_path, {}, routing={"review": ["m2", "m3"]})
    c.registry._models["m2"].callable = False
    c.registry._models["m3"].context_length = 10
    usable, skipped = c.router.candidates("review", min_context=1000)
    assert usable == ["m1"] and len(skipped) == 2  # only the user's own model is left: last resort for every role


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


def test_plan_wide_usage_limit_is_not_retried_but_another_provider_may_help():
    e = classify(429, "You've reached your 5-hour usage limit for your plan. Your limit resets at 19:12")
    assert not e.retry and e.fallback and e.quota
    assert classify(429, "slow down").retry  # ordinary burst limits still retry


async def test_quota_exhausted_gateway_falls_over_to_codex_and_stays_off_the_gateway(tmp_path):
    limit = classify(429, "You've reached your 5-hour usage limit for your plan")
    c, fake = make_client(tmp_path, {"m1": [limit], "codex/x": ["ok", "ok"]}, routing={"default": ["m2", "codex/x"]})
    c.registry._models["codex/x"] = ModelInfo("codex/x", 1_000_000)
    evs = await collect(c)
    assert any("Falling back from m1 to codex/x" in (e.notice or "") for e in evs)  # m2 shares m1's quota: skipped
    assert [m for m, _ in fake.calls] == ["m1", "codex/x"]
    evs = await collect(c)  # next request does not touch the gateway again
    assert not any("Falling back" in (e.notice or "") for e in evs)


class CappedFake(OpenAIChatAdapter):
    """Looks like a DeepSeek server: refuses any max_tokens above 8192."""

    def __init__(self):
        self.caps = []

    async def stream(self, model, messages, tools, temperature, max_tokens=None):
        self.caps.append(max_tokens)
        if max_tokens and max_tokens > 8192:
            raise classify(400, f"Invalid max_tokens value, the valid range of max_tokens is [1, 8192]")
        yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE)


async def test_output_cap_is_sent_and_a_server_limit_is_learned(tmp_path):
    c, _ = make_client(tmp_path, {})
    fake = CappedFake()
    c._adapter = lambda endpoint: fake
    await collect(c)
    await collect(c)
    assert fake.caps == [32_000, 8192, 8192]  # refused once, then its own number every time


def test_output_cap_shrinks_to_the_room_left(tmp_path):
    c, _ = make_client(tmp_path, {})
    assert c._output_cap("m1", 128_000, 110_000) == 128_000 - 110_000 - 4096
    assert c._output_cap("m1", 1_000_000, 1000) == 32_000


async def test_thinking_is_collected_from_the_stream():
    def think(text):
        return NS(usage=None, choices=[NS(delta=NS(content=None, tool_calls=None, reasoning_content=text), finish_reason=None)])

    async def gen():
        yield think("check the ")
        yield think("callers first")
        yield chunk(content="ok", finish="stop")

    evs = [e async for e in OpenAIChatAdapter("k", "http://x")._consume(gen())]
    assert evs[-1].reasoning == "check the callers first"


def test_thinking_goes_back_only_to_its_own_model_and_the_history_stays_append_only():
    from client.adapters import _wire_messages

    msgs = [
        {"role": "user", "content": "old question"},
        {"role": "assistant", "content": "a", "reasoning_content": "old", "reasoning_model": "deepseek-v4"},
        {"role": "user", "content": "fix it"},
        {"role": "assistant", "content": "", "reasoning_content": "plan", "reasoning_model": "deepseek-v4", "tool_calls": []},
        {"role": "tool", "tool_call_id": "c1", "content": "x"},
    ]
    same = _wire_messages(msgs, "deepseek-v4")
    assert same[1]["reasoning_content"] == "old" and same[3]["reasoning_content"] == "plan"
    # a harness note (a user message) must not rewrite earlier turns: that would miss the cache from there on
    assert _wire_messages(msgs + [{"role": "user", "content": "<system-reminder>x</system-reminder>"}], "deepseek-v4")[:5] == same
    assert not any("reasoning_model" in m for m in same)  # our bookkeeping never reaches the server
    assert "reasoning_content" not in _wire_messages(msgs, "kimi-k3")[3]  # another model can't use it
    assert msgs[3]["reasoning_model"] == "deepseek-v4"  # the stored history is not modified


@pytest.mark.parametrize("msg,overflow", [
    ("This model's maximum context length is 131072 tokens. However, you requested 140000 tokens", True),
    ("the request exceeds the available context size, try increasing it", True),  # llama.cpp
    ("Your request exceeded model token limit: 262144 (requested: 270000)", True),  # Kimi
    ("Range of input length should be [1, 129024]", True),  # DashScope / Qwen
    ("ThrottlingException: Too many tokens, please wait before trying again.", False),  # a rate limit
])
def test_context_overflow_is_recognised_across_providers(msg, overflow):
    from agent.agent import NOT_OVERFLOW, OVERFLOW

    assert bool(OVERFLOW.search(msg) and not NOT_OVERFLOW.search(msg)) is overflow


def test_quota_text_blocks_the_provider_whatever_the_status():
    assert classify(403, "You exceeded your current quota").quota


def test_sampling_follows_the_family_unless_a_temperature_is_set():
    from client.llm_client import sampling

    assert sampling("Qwen/Qwen3.8-Max", None) == (0.55, 1.0)
    assert sampling("moonshotai/Kimi-K3", None) == (1.0, 0.95)
    assert sampling("deepseek/deepseek-v4-pro", None) == (None, None)  # the server's own default
    assert sampling("Qwen/Qwen3.8-Max", 0.2) == (0.2, None)


def test_thinking_switch_is_worded_for_the_server_it_goes_to():
    from client.llm_client import thinking_body

    assert thinking_body("deepseek", "deepseek-v4-pro", "low") == {"thinking": {"type": "disabled"}}
    assert thinking_body("zai", "glm-5.3", "high") == {"thinking": {"type": "enabled", "clear_thinking": False}}
    assert thinking_body("openrouter", "qwen/qwen3.8-max", "high") == {"reasoning": {"effort": "high"}}
    assert thinking_body("mybox", "Qwen/Qwen3.8-27B", "low") == {"chat_template_kwargs": {"enable_thinking": False}}  # own vLLM
    assert thinking_body("groq", "openai/gpt-oss-120b", "high") == {"reasoning_effort": "high"}
    assert thinking_body("gateway", "deepseek/deepseek-v4-pro", "high") == {}  # the gateway ignores every switch
    assert thinking_body("deepseek", "deepseek-v4-pro", None) == {}  # an ordinary request keeps the server default


class ThinkingFake(OpenAIChatAdapter):
    def __init__(self):
        self.bodies = []

    async def stream(self, model, messages, tools, temperature, max_tokens=None, top_p=None, extra_body=None):
        self.bodies.append(extra_body)
        if extra_body:
            raise classify(400, "Unrecognized request argument supplied: thinking")
        yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE)


async def test_a_server_without_the_switch_is_asked_again_without_it_and_remembered(tmp_path, monkeypatch):
    c, _ = make_client(tmp_path, {})
    fake = ThinkingFake()
    c._adapter = lambda endpoint: fake
    monkeypatch.setattr(c, "_provider", lambda m: "deepseek")
    c.auto_effort = "high"
    await collect(c)
    await collect(c)
    assert fake.bodies == [{"thinking": {"type": "enabled"}}, None, None]


async def test_a_runaway_reply_is_stopped_and_its_open_call_comes_back_to_be_resent_smaller(tmp_path):
    from client.response import ToolCallDelta

    class Runaway(FakeAdapter):
        async def stream(self, model, messages, tools, temperature):
            yield StreamEvent(type=StreamEventType.TOOL_CALL_START, tool_call_delta=ToolCallDelta("c1", "edit"))
            for _ in range(1000):  # far more than the cap; the loop must stop reading well before the end
                self.calls.append(model)
                yield StreamEvent(type=StreamEventType.TOOL_CALL_DELTA, tool_call_delta=ToolCallDelta("c1", None, "x" * 1000))

    c, fake = make_client(tmp_path, {})
    c.config.model.max_output_tokens = 1000  # 4000 chars
    runaway = Runaway({})
    c._adapter = lambda endpoint: runaway
    evs = await collect(c)
    assert len(runaway.calls) < 10
    done = [e for e in evs if e.type == StreamEventType.MESSAGE_COMPLETE][-1]
    assert done.finish_reason == "length"
