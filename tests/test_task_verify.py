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


def test_related_tests_expand_only_for_specs_that_import_the_changed_file(tmp_path):
    from config.config import Config, VerifyRule
    from agent.verify import select_checks

    (tmp_path / "app" / "unit").mkdir(parents=True)
    (tmp_path / "app" / "unit" / "a.spec.ts").write_text('import { x } from "../src/lib/thing";\n')
    (tmp_path / "app" / "unit" / "b.spec.ts").write_text('import { y } from "../src/lib/other";\n')
    rule = VerifyRule(paths=["app/"], cwd="app", tests_glob="unit/**/*.spec.ts", commands=["run {tests}"])
    cfg = Config(cwd=tmp_path, verify_rules=[rule], auto_verify=False)
    assert select_checks(cfg, ["app/src/lib/thing.ts"]) == [("run unit/a.spec.ts", "app")]
    assert select_checks(cfg, ["app/src/lib/none.ts"]) == []


async def test_backup_and_undo_restore_edits_and_remove_created_files(tmp_path):
    import subprocess
    from agent.verify import backup, undo

    def git(*a):
        subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a], cwd=tmp_path, check=True, capture_output=True)

    git("init", "-q")
    (tmp_path / "a.txt").write_text("one\n")
    git("add", "-A"); git("commit", "-qm", "init")
    (tmp_path / "a.txt").write_text("two (user's unsaved work)\n")  # dirty tree before the run
    ref = await backup(tmp_path)
    (tmp_path / "a.txt").write_text("agent edit\n")
    (tmp_path / "new.txt").write_text("made by the run\n")
    restored, deleted, skipped = await undo(tmp_path, ref, ["a.txt", "new.txt"], ["new.txt"])
    assert (tmp_path / "a.txt").read_text() == "two (user's unsaved work)\n"
    assert not (tmp_path / "new.txt").exists() and restored == ["a.txt"] and deleted == ["new.txt"] and not skipped
    assert (await backup(tmp_path)) and subprocess.run(["git", "rev-parse", "refs/melon/backup"], cwd=tmp_path).returncode == 0


async def test_a_note_typed_mid_check_stops_the_check_and_is_worked_on_now(repo):
    import time

    # the first check would hang for 30s; the user's note comes in while it runs
    cfg = cfg_for(repo, verify_commands=["test -f steered.txt || sleep 30"])
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([write("a.txt"), text("done"), write("steered.txt"), text("fixed the buttons too")])
        t, events, steered = time.monotonic(), [], False
        async for e in a.run("go"):
            events.append(e)
            if e.type == AgentEventType.NOTICE and "Verifying" in str(e.data) and not steered:
                steered = a.steering.append("the white buttons are broken too") or True
        rep = final(events)
        assert time.monotonic() - t < 15
        assert rep["status"] == "verified" and rep["repairs"] == 0
        assert "white buttons" in str(a.session.client.requests[2][-1]["content"])


async def test_a_dropped_parameter_is_sent_back_once_even_though_the_checks_pass(repo):
    # the benchmark miss: the fix removed `seen` and the agent's own test still passed
    (repo / "lib.py").write_text("def unique_by(items, key, seen=set()):\n    return items\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", "lib")
    cfg = cfg_for(repo, verify_commands=["true"])
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([
            call("read_file", {"path": "lib.py"}),
            call("write_file", {"path": "lib.py", "content": "def unique_by(items, key):\n    return items\n"}), text("done"),
            call("write_file", {"path": "lib.py", "content": "def unique_by(items, key, seen=None):\n    return items\n"}), text("kept it"),
        ])
        rep = final(await run(a, "fix the shared state bug in unique_by"))
        sent = a.session.client.requests[3][-1]["content"]
        assert "unique_by() no longer accepts `seen`" in sent and rep["status"] == "verified"
        assert len(a.session.client.requests) == 5  # one nudge, not a loop


async def test_signature_guard_skips_what_the_user_asked_for_and_functions_that_moved(repo):
    from agent.verify import signature_breaks
    (repo / "a.py").write_text("def load(path, cache=None):\n    pass\n\ndef helper(x):\n    pass\n")
    (repo / "b.py").write_text("")
    (repo / "test_a.py").write_text("def test_x(tmp):\n    pass\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", "base")
    (repo / "a.py").write_text("def load(path):\n    pass\n")
    (repo / "b.py").write_text("def helper(x):\n    pass\n")  # moved, not removed
    (repo / "test_a.py").write_text("def test_x():\n    pass\n")  # tests are nobody's API
    changed = ["a.py", "b.py", "test_a.py"]
    assert await signature_breaks(repo, changed, "tidy up a.py") == ["a.py: load() no longer accepts `cache`"]
    assert await signature_breaks(repo, changed, "drop the cache parameter from load") == []
