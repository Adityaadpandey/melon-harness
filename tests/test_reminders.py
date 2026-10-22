"""Facts that change mid-run reach the model as short reminders after the history."""
import pytest

from agent.agent import Agent
from config.config import ApprovalPolicy, Config
from tests.fakes import ScriptedClient, call, calls, text


@pytest.fixture
def cfg(tmp_path):
    return Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO)


async def run(agent, msg="go"):
    return [e async for e in agent.run(msg)]


async def test_a_file_changed_behind_the_agents_back_is_reported_once(cfg):
    (cfg.cwd / "a.txt").write_text("one\n")
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([
            call("read_file", {"path": "a.txt"}),
            call("shell", {"command": "echo two >> a.txt"}, "c2"),  # a formatter or the user would do the same
            call("glob", {"pattern": "*.txt"}, "c3"),
            text("done"),
        ])
        await run(agent)
        reqs = agent.session.client.requests
        assert "Changed on disk" in reqs[2][-1]["content"] and "`a.txt`" in reqs[2][-1]["content"]
        assert not any("Changed on disk" in (m.get("content") or "") for m in reqs[3][len(reqs[2]):])  # not again


async def test_a_tool_call_typed_as_text_is_sent_back(cfg):
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([
            text('I will read it.\n<tool_call>{"name": "read_file", "arguments": {"path": "a.py"}}</tool_call>'),
            text("done"),
        ])
        await run(agent)
        assert "tool call as text" in agent.session.client.requests[1][-1]["content"]


async def test_an_untouched_task_list_is_brought_back(cfg):
    plan = call("todos", {"action": "set", "items": [{"content": "read the code"}, {"content": "fix it"}, {"content": "test it"}]})
    lookups = [call("glob", {"pattern": f"*{i}.py"}, f"g{i}") for i in range(11)]
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([plan, *lookups, text("done")])
        await run(agent)
        sent = [m.get("content") or "" for m in agent.session.client.requests[-1]]
        assert any("task list has not been touched" in c and "fix it" in c for c in sent)
