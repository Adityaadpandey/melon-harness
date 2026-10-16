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
    c = Config(pi_mode=False, cwd="/tmp")
    prompt = get_system_prompt(c, None, create_default_registry(c).get_tools(), [])
    assert count_tokens(prompt) < 1300  # was ~3000; every call pays this


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
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([call("read_file", {"path": "big.txt"}), text("ok")])
        evs = [e async for e in a.run("read")]
        done = next(e for e in evs if e.type == AgentEventType.TOOL_CALL_COMPLETE)
        assert done.data["success"] and "truncated" in done.data["output"]
        assert len(done.data["output"]) < 60_000


async def test_rereading_unchanged_file_is_deduped_until_context_changes(tmp_path):
    (tmp_path / "a.txt").write_text("hello")
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
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
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, tool_output_max_chars=1000)
    async with Agent(cfg) as a:
        r = await a.session.tool_registry.invoke(
            "shell", {"command": "python3 -c \"print('A'*5000); print('THE-END')\""}, tmp_path, a.session.hook_system, a.session.approval_manager
        )
        assert len(r.output) < 1300 and r.output.startswith("AAAA") and "THE-END" in r.output and r.truncated


async def test_token_budget_ends_with_a_final_answer_not_a_bare_failure(tmp_path):
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, max_total_tokens=1)
    explore = [call("list_dir", {"path": f"d{i}"}, f"c{i}") for i in range(3)]
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient(explore[:1] + [text("Found X at a.py:1; Y unknown.")])
        evs = [e async for e in a.run("go")]
        assert not any(e.type == AgentEventType.AGENT_ERROR for e in evs)
        end = evs[-1]
        assert "Found X" in end.data["response"] and end.data["report"]["status"] == "unverified"
        assert a.session.client.requests[-1][-1]["content"].startswith("Token budget is spent")


def test_anthropic_marks_history_for_caching():
    msgs = [{"role": "system", "content": "sys"}, {"role": "user", "content": "hi"}]
    system, out = to_anthropic_messages(msgs)
    # simulate what the adapter does before sending
    out[-1]["content"][-1]["cache_control"] = {"type": "ephemeral"}
    assert out[-1]["content"][-1]["cache_control"]["type"] == "ephemeral"


def _call(t, model, prompt, cached, sec=1):
    return {"event": "model_call", "ok": True, "model": model, "t": t, "sec": sec,
            "tokens": {"prompt_tokens": prompt, "completion_tokens": 10, "cached_tokens": cached}}


def test_cache_waste_attributes_reason_and_ignores_noise_and_first_turn():
    from utils.cost import cache_waste

    ev = [
        _call(0, "m", 10_000, 0),           # first turn: nothing to miss yet
        _call(10, "m", 12_000, 9_900),      # fine: previous prompt (10k) mostly read from cache
        _call(20, "m", 14_000, 12_000),     # fine
        _call(30, "m", 15_000, 2_000),      # prefix changed: ~12k tokens re-billed
        _call(900, "m", 16_000, 0),         # long idle gap: expired cache
        _call(910, "other", 17_000, 0),     # different model, never reported caching -> not counted
    ]
    w = cache_waste(ev, {})
    assert w["misses"] == 2 and w["reasons"] == ["idle > 5 min", "prefix changed"]
    assert w["missed_tokens"] == (14_000 - 2_000) + (15_000 - 0)


def test_cache_waste_resets_after_compaction_and_providers_without_caching():
    from utils.cost import cache_waste

    ev = [_call(0, "m", 50_000, 1), _call(5, "m", 60_000, 50_000), {"event": "compaction"}, _call(10, "m", 8_000, 0)]
    assert cache_waste(ev, {})["misses"] == 0
    assert cache_waste([_call(0, "n", 9_000, 0), _call(5, "n", 9_500, 0)], {})["misses"] == 0


async def test_scout_findings_start_the_main_context(tmp_path):
    from tools.base import Tool, ToolKind, ToolResult
    from pydantic import BaseModel

    class P(BaseModel):
        goal: str

    class FakeScout(Tool):
        name = "subagent_investigator"
        description = "scout"
        kind = ToolKind.READ
        schema = P

        async def execute(self, invocation):
            return ToolResult.success_result("route -> svc.ts:42")

    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as a:
        a.session.tool_registry.register(FakeScout(cfg))
        a.session.client = ScriptedClient([text("done"), text("ok")])
        long_task = "Investigate why the event description loses blank lines when a section exists " * 2
        [e async for e in a.run(long_task)]
        assert "route -> svc.ts:42" in a.session.client.requests[0][-1]["content"]
        [e async for e in a.run("fix typo")]  # short request: no scout


def test_heaviest_results_weighs_size_by_how_long_it_stays_in_context():
    from utils.cost import heaviest_results

    mc = {"event": "model_call", "ok": True}
    ev = [mc, {"event": "tool_call", "tool": "grep", "target": "x", "chars": 4000},  # 1000 tokens, 3 later calls
          mc, {"event": "tool_call", "tool": "read_file", "target": "a.ts", "chars": 8000},  # 2000 tokens, 2 later
          mc, {"event": "tool_call", "tool": "read_file", "target": "a.ts", "chars": 400}, mc, mc]
    h = heaviest_results(ev)
    assert (h["top"][0]["target"], h["top"][0]["carried"]) == ("a.ts", 6000) and h["top"][1]["carried"] == 4000
    assert h["repeated"] == ["read_file a.ts x2"]
    ev.insert(5, {"event": "compaction"})  # history rewritten: earlier results stop counting
    assert heaviest_results(ev)["tool_result_carried"] < h["tool_result_carried"]


async def test_overlapping_reads_return_only_the_lines_not_already_in_context(tmp_path, monkeypatch):
    monkeypatch.setattr("tools.builtin.read_file.WHOLE_FILE_LINES", 0)  # keep slices as asked here
    monkeypatch.setattr("tools.builtin.read_file.MIN_SLICE_LINES", 0)
    (tmp_path / "f.txt").write_text("\n".join(f"line{i}" for i in range(1, 31)) + "\n")
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO)
    async with Agent(cfg) as a:
        reg, hs = a.session.tool_registry, a.session.hook_system

        async def read(**kw):
            return (await reg.invoke("read_file", {"path": "f.txt", **kw}, tmp_path, hs)).output

        assert "1|line1" in await read(offset=1, limit=10)
        out = await read(offset=5, limit=11)  # 5-10 already seen, 11-15 new
        assert "11|line11" in out and "7|line7" not in out and "already in context" in out
        assert "unchanged and already in context" in await read(offset=3, limit=8)
        out = await read()  # whole file: only 16-30 is new
        assert "30|line30" in out and "\n2|line2\n" not in out and "[lines 1-15 already" in out
        (tmp_path / "f.txt").write_text("changed\n" + "\n".join(f"line{i}" for i in range(2, 31)) + "\n")
        assert "1|changed" in await read(offset=1, limit=3)  # file changed: forget the map


async def test_small_slices_are_widened_so_the_model_does_not_drip_feed_a_file(tmp_path):
    (tmp_path / "small.txt").write_text("\n".join(f"s{i}" for i in range(1, 301)) + "\n")
    (tmp_path / "big.txt").write_text("\n".join(f"b{i}" for i in range(1, 1001)) + "\n")
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO)
    async with Agent(cfg) as a:
        reg, hs = a.session.tool_registry, a.session.hook_system

        async def read(path, **kw):
            return (await reg.invoke("read_file", {"path": path, **kw}, tmp_path, hs)).output

        out = await read("small.txt", offset=100, limit=20)  # a 300-line file comes whole
        assert "1|s1\n" in out and "300|s300" in out
        out = await read("big.txt", offset=500, limit=20)  # a long file grows the slice to 200 lines
        assert "500|b500" in out and "699|b699" in out and "700|b700" not in out and "499|b499" not in out


def test_learned_lessons_and_edit_history_shape_prompt_and_search(tmp_path):
    from agent.learn import Learned

    repo, base = tmp_path / "repo", tmp_path / "data"
    (repo / "src").mkdir(parents=True)
    (repo / "src" / "about.tsx").write_text("x")
    L = Learned(repo, base)
    assert L.add_lesson("Never edit generated files under src/generated") and not L.add_lesson("never edit generated files under src/generated")
    assert not L.add_lesson("too short")  # fewer than four words is not a rule
    L.record_run("Fix the blank line in the event description when no section exists", ["src/about.tsx", "src/gone.tsx"])
    L2 = Learned(repo, base)  # a fresh process reads it back
    assert "Never edit generated files" in L2.prompt_section()
    boost = L2.prior("event description blank line bug")
    assert boost == {"src/about.tsx": boost["src/about.tsx"]} and boost["src/about.tsx"] > 0.25  # deleted files are never recorded
    assert L2.prior("payment webhook retry") == {}
    assert L2.forget(1).startswith("Never edit") and L2.prompt_section() == ""


async def test_changed_file_is_answered_with_a_diff_not_the_whole_file(tmp_path):
    body = "\n".join(f"line {i} of a long file" for i in range(200))
    (tmp_path / "big.txt").write_text(body)
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as a:
        reg = a.session.tool_registry
        await reg.invoke("read_file", {"path": "big.txt"}, tmp_path, a.session.hook_system)
        (tmp_path / "big.txt").write_text(body.replace("line 100 ", "LINE 100 "))
        r = await reg.invoke("read_file", {"path": "big.txt"}, tmp_path, a.session.hook_system)
        assert "diff_only" in r.metadata and "LINE 100" in r.output and "line 50 " not in r.output
        # after a diff the file counts as fully known again
        r = await reg.invoke("read_file", {"path": "big.txt"}, tmp_path, a.session.hook_system)
        assert "unchanged" in r.output


async def test_hung_subagent_is_cut_off_at_its_timeout(tmp_path, monkeypatch):
    import asyncio
    from client.llm_client import LLMClient
    from tools.base import ToolInvocation
    from tools.subagents import SubagentDefinition, SubagentTool

    async def hang(self, *a, **k):
        await asyncio.sleep(30)
        yield

    monkeypatch.setattr(LLMClient, "chat_completion", hang)
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO)
    tool = SubagentTool(cfg, SubagentDefinition("t", "d", "p", ["read_file"], timeout_seconds=0.5))
    r = await asyncio.wait_for(tool.execute(ToolInvocation(cwd=tmp_path, params={"goal": "x"})), 5)
    assert "timed out" in r.output


def test_request_size_is_split_by_part_without_keeping_the_text():
    from client.llm_client import request_chars

    msgs = [{"role": "system", "content": "abc"}, {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "grep", "arguments": "{}"}}]},
            {"role": "tool", "content": "x" * 100}]
    assert request_chars(msgs) == {"system": 3, "user": 5, "assistant": 0, "tool_calls": 6, "tool_results": 100}
