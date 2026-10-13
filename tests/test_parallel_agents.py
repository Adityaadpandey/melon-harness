import asyncio
import subprocess
import time

import pytest

import agent.orchestrator as orch
from agent.agent import Agent
from agent.events import AgentEvent, AgentEventType
from client.llm_client import LLMClient
from config.config import ApprovalPolicy, Config, SubagentSpec
from tests.fakes import ScriptedClient, calls, text
from tests.fakes import text as scripted_text

READ_ONLY = ["read_file", "grep", "glob", "list_dir"]


def spec(name, tools=READ_ONLY):
    return SubagentSpec(name=name, description=f"{name} agent", goal_prompt="do it", allowed_tools=tools)


async def run(cfg, script, progress=None):
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient(script)
        a.session.tool_registry.progress = progress
        return [e async for e in a.run("go")]


@pytest.fixture
def slow_child(monkeypatch):
    state = {"active": 0, "peak": 0}

    async def fake_chat(self, messages, tools=None, stream=True, role="default"):
        state["active"] += 1
        state["peak"] = max(state["peak"], state["active"])
        await asyncio.sleep(0.4)
        state["active"] -= 1
        for ev in scripted_text("child result"):
            yield ev

    monkeypatch.setattr(LLMClient, "chat_completion", fake_chat)
    return state


async def test_read_only_subagents_in_one_turn_run_concurrently(tmp_path, slow_child):
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.ON_REQUEST, auto_verify=False, check_references=False,
                 subagents=[spec("a"), spec("b"), spec("c")])
    t = time.perf_counter()
    evs = await run(cfg, [calls(*[(f"subagent_{n}", {"goal": f"look {n}"}) for n in "abc"]), text("merged")])
    elapsed = time.perf_counter() - t
    assert elapsed < 0.9 and slow_child["peak"] == 3  # serial would take >= 1.2s
    outs = [e.data["output"] for e in evs if e.type == AgentEventType.TOOL_CALL_COMPLETE]
    assert len(outs) == 3 and all("child result" in o for o in outs)  # no approval prompt was needed (read-only)


async def test_the_concurrency_cap_is_shared_and_respected(tmp_path, slow_child):
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, check_references=False,
                 max_parallel_agents=2, subagents=[spec("a"), spec("b"), spec("c")])
    t = time.perf_counter()
    await run(cfg, [calls(*[(f"subagent_{n}", {"goal": "x"}) for n in "abc"]), text("done")])
    assert slow_child["peak"] == 2 and time.perf_counter() - t >= 0.75  # 3 jobs, 2 slots: two waves


async def test_subagents_that_can_run_commands_are_serialized_and_need_approval(tmp_path, slow_child):
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, check_references=False,
                 subagents=[spec("dbg", READ_ONLY + ["shell"]), spec("dbg2", READ_ONLY + ["shell"])])
    await run(cfg, [calls(("subagent_dbg", {"goal": "x"}), ("subagent_dbg2", {"goal": "y"})), text("done")])
    assert slow_child["peak"] == 1


async def test_progress_events_reach_the_ui_hook(tmp_path, monkeypatch):
    async def fake_chat(self, messages, tools=None, stream=True, role="default"):
        if len(messages) <= 2:
            for ev in calls(("list_dir", {"path": "."})):
                yield ev
        else:
            for ev in scripted_text("found it"):
                yield ev

    monkeypatch.setattr(LLMClient, "chat_completion", fake_chat)
    seen = []
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, check_references=False, subagents=[spec("inv")])
    await run(cfg, [calls(("subagent_inv", {"goal": "map it"}, )), text("ok")], progress=lambda cid, info: seen.append((cid, info["kind"], info.get("name"))))
    kinds = [k for _, k, _ in seen]
    assert kinds[0] == "start" and "tool" in kinds and kinds[-1] == "done"
    assert ("c0", "tool", "list_dir") in seen


class FakeWorker:
    def __init__(self, cfg, cb=None):
        self.cwd = cfg.cwd

    async def __aenter__(self):
        return self

    async def __aexit__(self, *a):
        pass

    async def run(self, prompt):
        await asyncio.sleep(0.3)
        name = "a.py" if "YOUR task: TASK-A" in prompt else "b.py"
        (self.cwd / name).write_text("x = 1\n")
        yield AgentEvent(AgentEventType.AGENT_END, {"report": {"status": "verified"}})


def git(p, *a):
    subprocess.run(["git", *a], cwd=p, check=True, capture_output=True)


async def test_parallel_tasks_tool_fans_out_into_worktrees_and_merges(tmp_path, monkeypatch):
    git(tmp_path, "init", "-q"); git(tmp_path, "config", "user.email", "t@t"); git(tmp_path, "config", "user.name", "t")
    (tmp_path / "README.md").write_text("r")
    git(tmp_path, "add", "-A"); git(tmp_path, "commit", "-qm", "i")
    monkeypatch.setattr(orch, "Agent", FakeWorker)
    asked = []

    async def approve(conf):
        asked.append(conf.description)
        return True

    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.ON_REQUEST, auto_verify=False, check_references=False)
    async with Agent(cfg, approve) as a:
        assert a.session.tool_registry.get("parallel_tasks") is not None
        a.session.client = ScriptedClient([
            calls(("parallel_tasks", {"tasks": [{"id": "a", "goal": "TASK-A make a.py", "files": ["a.py"]}, {"id": "b", "goal": "TASK-B make b.py", "files": ["b.py"]}]})),
            text("both done"),
        ])
        t = time.perf_counter()
        evs = [e async for e in a.run("do both")]
        assert time.perf_counter() - t < 2.0
    done = next(e for e in evs if e.type == AgentEventType.TOOL_CALL_COMPLETE)
    assert done.data["success"] and "Parallel run: ok" in done.data["output"] and "fast-forwarded" in done.data["output"]
    assert (tmp_path / "a.py").exists() and (tmp_path / "b.py").exists()
    assert asked and "Run 2 tasks in parallel" in asked[0]  # the user saw the plan before anything ran


async def test_parallel_tasks_is_not_offered_outside_git_or_inside_workers(tmp_path):
    from tools.registry import create_default_registry

    assert create_default_registry(Config(cwd=tmp_path)).get("parallel_tasks") is None
    git(tmp_path, "init", "-q")
    assert create_default_registry(Config(cwd=tmp_path)).get("parallel_tasks") is not None
    assert create_default_registry(Config(cwd=tmp_path, enable_parallel_tasks=False)).get("parallel_tasks") is None
