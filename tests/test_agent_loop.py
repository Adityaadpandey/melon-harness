import asyncio
import json
import time

import pytest

from agent.agent import Agent
from agent.events import AgentEventType
from config.config import ApprovalPolicy, Config
from client.response import TokenUsage
from tests.fakes import ScriptedClient, call, calls, error, text
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from pydantic import BaseModel


@pytest.fixture
def cfg(tmp_path):
    return Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO)


async def run(agent: Agent, msg="go"):
    return [e async for e in agent.run(msg)]


async def test_tool_call_arguments_are_valid_json_in_history(cfg):
    (cfg.cwd / "a.txt").write_text("hi")
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient(
            [call("read_file", {"path": "a.txt"}), text("done")]
        )
        await run(agent)
        sent = agent.session.client.requests[1]
        tc = next(m for m in sent if m.get("tool_calls"))["tool_calls"][0]
        assert json.loads(tc["function"]["arguments"]) == {"path": "a.txt"}


async def test_api_error_stops_loop_and_leaves_no_empty_assistant_turn(cfg):
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([error("boom")])
        events = await run(agent)
        assert [e.type for e in events if e.type == AgentEventType.AGENT_ERROR]
        assert agent.session.client.turns == []  # no second request was made
        roles = [m["role"] for m in agent.session.context_manager.get_messages()]
        assert roles == ["system", "user"]


async def test_malformed_arguments_are_rejected_not_executed(cfg):
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient(
            [call("write_file", {"raw_arguments": '{"path": "x'}), text("ok")]
        )
        events = await run(agent)
        done = next(e for e in events if e.type == AgentEventType.TOOL_CALL_COMPLETE)
        assert done.data["success"] is False
        assert "valid JSON" in done.data["error"]
        assert not (cfg.cwd / "x").exists()


class SleepParams(BaseModel):
    tag: str = "x"


class SleepTool(Tool):
    name = "sleepy"
    description = "sleeps"
    kind = ToolKind.READ
    schema = SleepParams

    async def execute(self, inv: ToolInvocation) -> ToolResult:
        await asyncio.sleep(0.2)
        return ToolResult.success_result(inv.params["tag"])


async def test_read_only_calls_run_concurrently_in_order(cfg):
    async with Agent(cfg) as agent:
        agent.session.tool_registry.register(SleepTool(cfg))
        agent.session.client = ScriptedClient(
            [calls(*[("sleepy", {"tag": str(i)}) for i in range(5)]), text("done")]
        )
        t = time.perf_counter()
        events = await run(agent)
        elapsed = time.perf_counter() - t
        assert elapsed < 0.6  # serial would be >= 1.0s
        outs = [e.data["output"] for e in events if e.type == AgentEventType.TOOL_CALL_COMPLETE]
        assert outs == ["0", "1", "2", "3", "4"]


async def test_writes_are_barriers_and_stay_ordered(cfg):
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient(
            [
                calls(
                    ("write_file", {"path": "f.txt", "content": "v1"}),
                    ("read_file", {"path": "f.txt"}),
                ),
                text("done"),
            ]
        )
        events = await run(agent)
        read = [e for e in events if e.type == AgentEventType.TOOL_CALL_COMPLETE][1]
        assert "v1" in read.data["output"]


async def test_no_callback_means_no_silent_approval(tmp_path):
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.ON_REQUEST)
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient(
            [call("shell", {"command": "touch n.txt"}), text("ok")]
        )
        await run(agent)
        assert not (tmp_path / "n.txt").exists()


def test_empty_tool_output_still_has_content_key():
    from context.manager import MessageItem

    m = MessageItem(role="tool", content="", tool_call_id="x").to_dict()
    assert m["content"] == "(no output)"
    assert MessageItem(role="assistant", content="", tool_calls=[{"id": "1"}]).to_dict()["content"] == ""


async def test_a_stalled_model_stream_ends_when_the_time_budget_does(tmp_path):
    import asyncio
    from agent.agent import _within

    async def stalled():
        yield "first"
        await asyncio.sleep(30)

    got = [e async for e in _within(0.2, stalled())]
    assert got[0] == "first" and "Time budget" in got[1].error


async def test_session_autosave_resume_and_continue_by_folder(tmp_path, monkeypatch):
    import agent.persistence as per
    from main import CLI

    monkeypatch.setattr(per, "get_data_dir", lambda: tmp_path / "data")
    work = tmp_path / "work"
    work.mkdir()
    cfg = Config(cwd=work, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as a:
        cli = CLI(cfg)
        cli.agent = a
        cm = a.session.context_manager
        cm.add_user_message("Fix the blank line bug in the description\n\n---\nPlaybooks selected: lots of text")
        cm.add_assistant_message(None, [{"id": "c1", "type": "function", "function": {"name": "grep", "arguments": "{}"}}])
        cm.add_tool_result("c1", "found it")
        a.session.increment_turn()
        cli._autosave()
        sid = a.session.session_id
    pm = per.PersistenceManager()
    row = pm.list_sessions()[0]
    assert row["title"] == "Fix the blank line bug in the description" and row["cwd"] == str(work)
    assert pm.latest_for(str(work)) == sid and pm.latest_for("/elsewhere") is None

    async with Agent(cfg) as b:
        cli2 = CLI(cfg)
        cli2.agent = b
        assert await cli2._resume(sid) and not await cli2._resume("nope")
        roles = [m["role"] for m in b.session.context_manager.get_messages()]
        assert roles[1:] == ["user", "assistant", "tool"] and b.session.session_id == sid and b.session.turn_count == 1


def test_old_session_files_without_cwd_still_load(tmp_path, monkeypatch):
    import json
    import agent.persistence as per

    monkeypatch.setattr(per, "get_data_dir", lambda: tmp_path / "data")
    pm = per.PersistenceManager()
    old = {"session_id": "old1", "created_at": "2026-01-01T00:00:00", "updated_at": "2026-01-01T00:00:00", "turn_count": 2,
           "messages": [], "total_usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0, "cached_tokens": 0}}
    (pm.sessions_dir / "old1.json").write_text(json.dumps(old))
    assert pm.load_session("old1").cwd == "" and pm.list_sessions()[0]["title"] == ""


async def test_an_identical_lookup_is_answered_once_until_something_is_written(cfg):
    (cfg.cwd / "a.py").write_text("needle = 1\n" * 50)
    async with Agent(cfg) as agent:
        g = ("grep", {"pattern": "needle", "path": "."})
        agent.session.client = ScriptedClient([
            call(*g, call_id="a"), call(*g, call_id="b"),
            call("write_file", {"path": "b.py", "content": "needle = 2\n"}, call_id="w"),
            call(*g, call_id="c"), text("done"),
        ])
        events = await run(agent)
        outs = {e.data["call_id"]: e.data["output"] for e in events if e.type == AgentEventType.TOOL_CALL_COMPLETE}
        assert "needle" in outs["a"] and "unchanged" in outs["b"]  # repeat: a pointer, not the same text again
        assert "needle" in outs["c"] and "unchanged" not in outs["c"]  # the write invalidated it


async def test_a_cleared_result_is_not_treated_as_still_in_context(cfg):
    (cfg.cwd / "a.py").write_text("needle = 1\n")
    async with Agent(cfg) as agent:
        g = ("grep", {"pattern": "needle", "path": "."})
        agent.session.client = ScriptedClient([call(*g, call_id="a"), call(*g, call_id="b"), text("done")])
        cm = agent.session.context_manager
        orig = cm.add_tool_result

        def add_then_clear(cid, content):
            orig(cid, content)
            cm._messages[-1].pruned_at = True  # as if pruning had cleared it at once

        cm.add_tool_result = add_then_clear
        events = await run(agent)
        outs = {e.data["call_id"]: e.data["output"] for e in events if e.type == AgentEventType.TOOL_CALL_COMPLETE}
        assert "unchanged" not in outs["b"]


async def test_two_workspaces_never_share_a_cached_answer(tmp_path):
    from tools.registry import create_default_registry
    from agent.agent import Agent  # noqa: F401

    for name, body in (("one", "needle one"), ("two", "needle two")):
        (tmp_path / name).mkdir()
        (tmp_path / name / "f.txt").write_text(body)
    cfg = Config(cwd=tmp_path / "one", approval=ApprovalPolicy.AUTO)
    reg = create_default_registry(cfg)
    from hooks.hook_system import HookSystem

    hooks = HookSystem(cfg)
    a = await reg.invoke("grep", {"pattern": "needle", "path": "."}, tmp_path / "one", hooks, None, "x1")
    b = await reg.invoke("grep", {"pattern": "needle", "path": "."}, tmp_path / "two", hooks, None, "x2")
    assert "one" in a.output and "two" in b.output and "unchanged" not in b.output


async def test_budget_counts_new_work_not_a_resent_cached_prefix(tmp_path):
    from client.response import StreamEvent, StreamEventType, TokenUsage

    (tmp_path / "a.txt").write_text("hi")
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, max_total_tokens=10_000)

    def turn(i):  # 100k prompt, all but 500 of it cached: huge in total, tiny as new work
        ev = call("read_file", {"path": "a.txt"}, call_id=f"c{i}")
        ev[-1] = StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, usage=TokenUsage(100_000, 50, 100_050, 99_500))
        return ev

    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([turn(i) for i in range(8)] + [text("done")])
        events = await run(agent)
        assert not [e for e in events if e.type == AgentEventType.NOTICE and "budget" in str(e.data).lower()]


async def test_a_run_stopped_by_the_budget_still_reports_the_files_it_changed(tmp_path):
    import subprocess
    from client.response import StreamEvent, StreamEventType, TokenUsage

    for a in (["init", "-q"], ["config", "user.email", "a@b"], ["config", "user.name", "t"]):
        subprocess.run(["git", *a], cwd=tmp_path, check=True, capture_output=True)
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, max_total_tokens=1000)
    ev = call("write_file", {"path": "new.txt", "content": "x"}, call_id="w")
    ev[-1] = StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, usage=TokenUsage(5000, 10, 5010, 0))
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([ev, text("partial answer")])
        events = await run(agent)
        report = next(e for e in events if e.type == AgentEventType.AGENT_END).data["report"]
        assert report["changed_files"] == ["new.txt"]


async def test_a_budget_stop_runs_the_checks_and_reports_a_broken_tree(tmp_path):
    import subprocess
    from client.response import StreamEvent, StreamEventType, TokenUsage

    for a in (["init", "-q"], ["config", "user.email", "a@b"], ["config", "user.name", "t"]):
        subprocess.run(["git", *a], cwd=tmp_path, check=True, capture_output=True)
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, max_total_tokens=1000, verify_commands=["exit 3"])
    ev = call("write_file", {"path": "new.txt", "content": "x"}, call_id="w")
    ev[-1] = StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, usage=TokenUsage(5000, 10, 5010, 0))
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([ev, text("partial answer")])
        report = next(e for e in await run(agent) if e.type == AgentEventType.AGENT_END).data["report"]
        assert report["status"] == "verification_failed" and report["checks"][0]["exit_code"] == 3


async def test_a_run_that_cannot_ask_for_approval_stops_at_the_first_refusal(tmp_path):
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.ON_REQUEST, auto_verify=False)
    turns = [call("write_file", {"path": "n.txt", "content": "x"}, call_id=f"w{i}") for i in range(5)]
    async with Agent(cfg) as agent:  # no confirmation callback: single-run mode
        agent.session.client = ScriptedClient(turns + [text("done")])
        events = await run(agent)
        errs = [e for e in events if e.type == AgentEventType.AGENT_ERROR]
        assert errs and "--approval auto-edit" in errs[0].data["error"]
        assert len(agent.session.client.requests) == 1  # one model call, not a retry loop
        assert not (tmp_path / "n.txt").exists()


async def test_steering_note_lands_after_the_tool_round_and_before_the_next_model_call(cfg):
    (cfg.cwd / "a.txt").write_text("hi")
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([call("read_file", {"path": "a.txt"}), text("done")])
        agent.steering.append("use python 3.12")
        await run(agent)
        second = agent.session.client.requests[1]
        assert "use python 3.12" in str(second[-1]["content"])
        assert agent.steering == []


async def test_provider_context_overflow_compacts_once_and_retries(cfg):
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([error("This model's maximum context length is 128000 tokens"), text("done")])
        for i in range(30):  # enough history for a compaction cut to exist
            agent.session.context_manager.add_user_message(f"old message {i} " * 200)
            agent.session.context_manager.add_assistant_message(f"old reply {i} " * 200, None)

        async def fake_compact():
            agent.session.context_manager.apply_compaction("summary", 20)
            return True

        agent._compact = fake_compact
        events = await run(agent)
        kinds = [e.type for e in events]
        assert AgentEventType.AGENT_ERROR not in kinds
        assert any("too long" in e.data.get("message", "") for e in events if e.type == AgentEventType.NOTICE)
        assert len(agent.session.client.requests) == 2


async def test_a_cold_cache_after_a_pause_shrinks_history_before_the_first_call(cfg):
    cfg.idle_compact_at_tokens = 1000
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([text("done")])
        compacted = []

        async def fake_compact():
            compacted.append(1)
            return True

        agent._compact = fake_compact
        agent.session.context_manager.set_latest_usage(TokenUsage(60_000, 100, 60_100))
        agent._last_model_call = time.monotonic() - 10_000  # long pause
        events = await run(agent)
        assert compacted and any("Cache had expired" in e.data.get("message", "") for e in events if e.type == AgentEventType.NOTICE)

        compacted.clear()
        agent._last_model_call = time.monotonic()  # warm cache: leave history alone
        agent.session.client = ScriptedClient([text("done")])
        await run(agent)
        assert not compacted
