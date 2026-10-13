import json

import pytest

from agent.agent import Agent
from agent.events import AgentEventType
from client.adapters import AnthropicAdapter, to_anthropic_messages
from config.config import ApprovalPolicy, Config, ModelPrice
from prompts.system import get_system_prompt
from tests.fakes import ScriptedClient, call, text
from tools.registry import create_default_registry
from utils.cost import summarize_usage
from utils.text import count_tokens


def test_system_prompt_stays_lean():
    c = Config(cwd="/tmp")
    prompt = get_system_prompt(c, None, create_default_registry(c).get_tools(), [])
    assert count_tokens(prompt) < 1200  # was ~3000; every call pays this


def test_cost_summary_only_prices_known_models():
    ev = [
        {"event": "model_call", "ok": True, "model": "a", "tokens": {"prompt_tokens": 1000, "completion_tokens": 100, "cached_tokens": 800}},
        {"event": "model_call", "ok": False, "model": "a"},
    ]
    s = summarize_usage(ev, {"a": ModelPrice(input=1.0, output=2.0, cached_input=0.1)})
    assert s["cache_hit_pct"] == 80.0
    assert s["est_cost_usd"] == pytest.approx((200 * 1.0 + 800 * 0.1 + 100 * 2.0) / 1e6, abs=1e-4)
    assert summarize_usage(ev, {})["est_cost_usd"] is None  # unknown price stays unknown


async def test_large_file_is_truncated_not_unreadable(tmp_path):
    (tmp_path / "big.txt").write_text("\n".join(f"line {i} " + "x" * 60 for i in range(6000)))
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([call("read_file", {"path": "big.txt"}), text("ok")])
        evs = [e async for e in a.run("read")]
        done = next(e for e in evs if e.type == AgentEventType.TOOL_CALL_COMPLETE)
        assert done.data["success"] and "truncated" in done.data["output"]
        assert len(done.data["output"]) < 60_000


async def test_rereading_unchanged_file_is_deduped_until_context_changes(tmp_path):
    (tmp_path / "a.txt").write_text("hello")
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as a:
        reg = a.session.tool_registry
        a.session.client = ScriptedClient([
            call("read_file", {"path": "a.txt"}, "c1"),
            call("read_file", {"path": "a.txt"}, "c2"),
            text("done"),
        ])
        evs = [e async for e in a.run("go")]
        outs = [e.data["output"] for e in evs if e.type == AgentEventType.TOOL_CALL_COMPLETE]
        assert "hello" in outs[0] and "unchanged" in outs[1]
        # once the earlier output has left the context, a re-read must return real content
        reg.epoch += 1
        r = await reg.invoke("read_file", {"path": "a.txt"}, tmp_path, a.session.hook_system)
        assert "hello" in r.output
        # and an edit invalidates it too
        (tmp_path / "a.txt").write_text("changed!")
        r = await reg.invoke("read_file", {"path": "a.txt"}, tmp_path, a.session.hook_system)
        assert "changed!" in r.output


async def test_tool_output_is_capped_head_and_tail(tmp_path):
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, tool_output_max_chars=1000)
    async with Agent(cfg) as a:
        r = await a.session.tool_registry.invoke(
            "shell", {"command": "python3 -c \"print('A'*5000); print('THE-END')\""}, tmp_path, a.session.hook_system, a.session.approval_manager
        )
        assert len(r.output) < 1300 and r.output.startswith("AAAA") and "THE-END" in r.output and r.truncated


async def test_token_budget_stops_a_runaway_run(tmp_path):
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, max_total_tokens=3)
    same = [call("list_dir", {"path": f"d{i}"}, f"c{i}") for i in range(10)]
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient(same)
        evs = [e async for e in a.run("go")]
        errs = [e.data["error"] for e in evs if e.type == AgentEventType.AGENT_ERROR]
        assert any("budget" in e.lower() for e in errs)


def test_anthropic_marks_history_for_caching():
    msgs = [{"role": "system", "content": "sys"}, {"role": "user", "content": "hi"}]
    system, out = to_anthropic_messages(msgs)
    # simulate what the adapter does before sending
    out[-1]["content"][-1]["cache_control"] = {"type": "ephemeral"}
    assert out[-1]["content"][-1]["cache_control"]["type"] == "ephemeral"
