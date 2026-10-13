import asyncio
import json
import time

import pytest

from agent.agent import Agent
from agent.events import AgentEventType
from config.config import ApprovalPolicy, Config
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
