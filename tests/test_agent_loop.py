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
