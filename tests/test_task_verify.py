import subprocess

import pytest

from agent.agent import Agent
from agent.events import AgentEventType
from agent.task import InvalidTransition, TaskState, TaskStatus
from config.config import ApprovalPolicy, Config
from tests.fakes import ScriptedClient, call, text
from utils.trace import Tracer


def git(path, *a):
    subprocess.run(["git", *a], cwd=path, check=True, capture_output=True)


@pytest.fixture
def repo(tmp_path):
    git(tmp_path, "init", "-q")
    git(tmp_path, "config", "user.email", "t@t")
    git(tmp_path, "config", "user.name", "t")
    (tmp_path / "keep.txt").write_text("x")
    git(tmp_path, "add", "-A")
    git(tmp_path, "commit", "-qm", "init")
    return tmp_path


def cfg_for(repo, **kw):
    return Config(cwd=repo, approval=ApprovalPolicy.AUTO, **kw)


async def run(agent, msg="go"):
    return [e async for e in agent.run(msg)]


def final(events):
    return next(e for e in events if e.type == AgentEventType.AGENT_END).data["report"]


def write(name):
    return call("write_file", {"path": name, "content": "1"})


def test_invalid_transitions_are_rejected():
    t = TaskState("x", Tracer("t"), 10, 1)
    t.transition(TaskStatus.FAILED)
    with pytest.raises(InvalidTransition):
        t.transition(TaskStatus.VERIFIED)


async def test_failed_check_triggers_repair_then_verifies(repo):
    cfg = cfg_for(repo, verify_commands=["test -f ok.txt"])
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient(
            [write("bad.txt"), text("done"), write("ok.txt"), text("fixed")]
        )
        rep = final(await run(a))
        assert rep["status"] == "verified" and rep["repairs"] == 1
        assert sorted(rep["changed_files"]) == ["bad.txt", "ok.txt"]
        # the failure output reached the model as the repair request
        repair_req = a.session.client.requests[2][-1]["content"]
        assert "verification FAILED" in repair_req.lower() or "FAILED" in repair_req


async def test_persistent_failure_is_reported_not_claimed_as_success(repo):
    cfg = cfg_for(repo, verify_commands=["false"], max_repairs=1)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([write("a.txt"), text("done"), text("tried")])
        rep = final(await run(a))
        assert rep["status"] == "verification_failed" and rep["repairs"] == 1


async def test_read_only_run_needs_no_verification(repo):
    cfg = cfg_for(repo, verify_commands=["false"])
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([text("the answer")])
        assert final(await run(a))["status"] == "done_no_changes"


async def test_preexisting_user_edits_are_not_blamed_on_the_agent(repo):
    (repo / "keep.txt").write_text("user edit in progress")
    cfg = cfg_for(repo, verify_commands=["false"])
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([text("nothing to do")])
        rep = final(await run(a))
        assert rep["status"] == "done_no_changes" and rep["changed_files"] == []


async def test_changes_without_any_checks_are_labelled_unverified(repo):
    cfg = cfg_for(repo, auto_verify=False)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([write("n.txt"), text("done")])
        assert final(await run(a))["status"] == "unverified"


async def test_repeated_identical_calls_end_the_task(repo):
    cfg = cfg_for(repo, auto_verify=False)
    same = [call("list_dir", {"path": "."}, f"c{i}") for i in range(12)]
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient(same)
        events = await run(a)
        assert final(events)["status"] == "failed"
        assert any("Stopped" in e.data.get("error", "") for e in events if e.type == AgentEventType.AGENT_ERROR)


async def test_code_pasted_instead_of_written_gets_one_nudge(repo):
    cfg = cfg_for(repo, auto_verify=False)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient(
            [text("Here you go:\n```py\nx=1\n```"), write("x.py"), text("applied")]
        )
        rep = final(await run(a, "create x.py with x = 1"))
        assert rep["status"] == "unverified" and rep["changed_files"] == ["x.py"]


async def test_nudge_happens_only_once(repo):
    cfg = cfg_for(repo, auto_verify=False)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([text("```py\na\n```"), text("```py\nb\n```")])
        assert final(await run(a, "create x.py"))["status"] == "done_no_changes"  # nudged once, then accepted
