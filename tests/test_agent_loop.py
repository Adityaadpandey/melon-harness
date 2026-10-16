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

    async with Agent(cfg, confirmation_callback=lambda c: True) as b:
        cli2 = CLI(cfg)
        cli2.agent = b
        ask = b.session.approval_manager.confirmation_callback
        assert await cli2._resume(sid) and not await cli2._resume("nope")
        assert b.session.approval_manager.confirmation_callback is ask  # a resumed session can still ask
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
    cfg.pi_mode = False  # idle tidying is part of the guarded setup
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


async def test_read_only_request_is_cut_off_at_the_call_budget_and_forced_to_answer(cfg):
    cfg.read_call_budget = 3
    (cfg.cwd / "a.txt").write_text("hi")
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([call("read_file", {"path": "a.txt"}, f"c{i}") for i in range(3)] + [text("answer")])  # the 4th reply is the forced wrap-up
        events = await run(agent, "explain what a.txt holds")
        assert sum(e.type == AgentEventType.TOOL_CALL_START for e in events) == 3
        assert any("Call budget reached" in e.data.get("message", "") for e in events if e.type == AgentEventType.NOTICE)
        assert any(e.type == AgentEventType.TEXT_COMPLETE for e in events)


async def test_requests_that_change_files_are_not_capped_by_the_read_budget(cfg):
    cfg.read_call_budget = 1
    (cfg.cwd / "a.txt").write_text("hi")
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([call("read_file", {"path": "a.txt"}, f"c{i}") for i in range(3)] + [text("done")])
        events = await run(agent, "fix the typo in a.txt")
        assert sum(e.type == AgentEventType.TOOL_CALL_START for e in events) == 3


async def test_a_run_that_starts_read_only_but_edits_is_no_longer_capped(cfg):
    cfg.read_call_budget = 2
    (cfg.cwd / "a.txt").write_text("hi")
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([
            call("edit", {"path": "a.txt", "old_string": "hi", "new_string": "yo"}, "e1"),
            call("read_file", {"path": "a.txt"}, "r1"), call("read_file", {"path": "a.txt"}, "r2"),
            call("read_file", {"path": "a.txt"}, "r3"), text("done"),
        ])
        events = await run(agent, "explain a.txt and why it is short")  # reads as read-only, then edits
        assert sum(e.type == AgentEventType.TOOL_CALL_START for e in events) == 4


async def test_more_allows_exactly_that_many_calls_for_one_request_only(cfg):
    cfg.read_call_budget = 20
    async with Agent(cfg) as agent:
        agent.extra_calls = 10
        agent.session.client = ScriptedClient([text("ok")])
        await run(agent, "explain things")
        assert agent._call_limit() == 10 and agent.extra_calls == 0
        agent.session.client = ScriptedClient([text("ok")])
        await run(agent, "explain more")
        assert agent._call_limit() == 20


async def test_a_capped_answer_with_unverifiable_refs_is_not_sent_back_for_a_second_answer(cfg):
    cfg.read_call_budget = 1
    cfg.check_references = True
    (cfg.cwd / "a.txt").write_text("hi")
    async with Agent(cfg) as agent:
        # the wrap-up cites a file that does not exist; with no tools left, a correction round would only cost another full-context call
        agent.session.client = ScriptedClient([call("read_file", {"path": "a.txt"}, "c0"), text("see ghost/missing.py:3"), text("second answer")])
        events = await run(agent, "explain what a.txt holds")
        assert sum(e.type == AgentEventType.TEXT_COMPLETE for e in events) == 1


async def test_parallel_calls_in_one_turn_count_as_one_round_of_the_read_budget(cfg):
    cfg.read_call_budget = 2
    for n in "abcde":
        (cfg.cwd / f"{n}.txt").write_text(n)
    async with Agent(cfg) as agent:
        # five reads at once, then two more single reads: 3 rounds, so the third is cut off
        agent.session.client = ScriptedClient([
            calls(*[("read_file", {"path": f"{n}.txt"}) for n in "abcde"]),
            call("read_file", {"path": "a.txt"}, "r1"), call("read_file", {"path": "b.txt"}, "r2"), text("answer"),
        ])
        events = await run(agent, "explain what these files hold")
        assert sum(e.type == AgentEventType.TOOL_CALL_START for e in events) == 6
        assert any("Call budget reached (2 tool rounds)" in e.data.get("message", "") for e in events if e.type == AgentEventType.NOTICE)


async def test_time_spent_waiting_for_approval_does_not_use_up_the_time_budget(tmp_path):
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.ON_REQUEST, max_seconds=0.5)

    async def slow_user(_):  # the user is away for longer than the whole budget, then approves
        await asyncio.sleep(0.8)
        return True

    async with Agent(cfg, confirmation_callback=slow_user) as agent:
        agent.session.client = ScriptedClient([call("shell", {"command": "touch made.txt"}), text("done")])
        events = await run(agent, "create made.txt")
        assert not any(e.type == AgentEventType.AGENT_ERROR for e in events)
        assert (tmp_path / "made.txt").exists() and agent.task.paused >= 0.8


async def test_running_out_of_time_still_produces_an_answer(cfg):
    cfg.max_seconds = 0.0001
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([text("here is what I know")])
        events = await run(agent, "explain the repo")
        assert any("Time budget reached" in e.data.get("message", "") for e in events if e.type == AgentEventType.NOTICE)
        assert any(e.type == AgentEventType.TEXT_COMPLETE for e in events)


async def test_silent_rounds_get_a_write_down_checkpoint_and_a_fix_now_nudge(cfg):
    for i in range(16):  # different files each round, so the loop detector stays quiet
        (cfg.cwd / f"f{i}.txt").write_text(f"file {i}")
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([call("read_file", {"path": f"f{i}.txt"}, f"c{i}") for i in range(16)] + [text("done")])
        await run(agent, "review and fix the files")
        said = [m.content for m in agent.session.context_manager._messages if m.role == "user"]
        assert sum("Checkpoint:" in (c or "") for c in said) == 2  # after rounds 8 and 16
        assert sum("nothing is changed yet" in (c or "") for c in said) == 1  # once, at round 15


async def test_a_review_that_argues_with_itself_is_rewritten_without_another_loop_round(cfg):
    cfg.read_call_budget = 2  # the one read below has used half of it when the answer comes
    (cfg.cwd / "a.txt").write_text("hi")
    messy = "high a.txt:1 keys clash. Actually a non-issue. Skip.\nlow a.txt:1 extra work. Acceptable; not a bug."
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([call("read_file", {"path": "a.txt"}), text(messy), text("high a.txt:1 write can 500 the page -> catch")])
        events = await run(agent, "review a.txt")
        done = [e.data.get("content") for e in events if e.type == AgentEventType.TEXT_COMPLETE]
        assert done[-1] == "high a.txt:1 write can 500 the page -> catch"
        assert not any("Call budget" in e.data.get("message", "") for e in events if e.type == AgentEventType.NOTICE)


async def test_a_review_stopped_by_the_call_cap_is_asked_for_the_review_not_a_next_step_summary_and_still_gets_the_rewrite(cfg):
    cfg.read_call_budget = 1
    (cfg.cwd / "a.txt").write_text("hi")
    messy = "HIGH a.txt:1 two ids collide. Finding retracted; moot."
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([call("read_file", {"path": "a.txt"}), text(messy), text("HIGH a.txt:1 a failing write 500s the page -> catch")])
        events = await run(agent, "review a.txt")
        asked = [m.content for m in agent.session.context_manager._messages if m.role == "user"]
        assert any("Answer now with the review itself" in (c or "") for c in asked)
        assert not any("next step" in (c or "") for c in asked)
        done = [e.data.get("content") for e in events if e.type == AgentEventType.TEXT_COMPLETE]
        assert done[-1].startswith("HIGH a.txt:1 a failing write")


async def test_a_review_near_the_call_cap_is_told_to_verify_its_top_two_candidates(cfg):
    cfg.read_call_budget = 4
    for i in range(4):
        (cfg.cwd / f"f{i}.txt").write_text(f"file {i}")
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([call("read_file", {"path": f"f{i}.txt"}, f"c{i}") for i in range(3)] + [text("No issues found")])
        await run(agent, "review the files")
        said = [m.content or "" for m in agent.session.context_manager._messages if m.role == "user"]
        assert any("two candidates most likely to be real defects" in c for c in said)


async def test_todo_set_replaces_the_list_in_one_call_and_survives_a_clear(cfg):
    async with Agent(cfg) as agent:
        todos = agent.session.tool_registry.get("todos")
        res = await todos.execute(type("I", (), {"params": {"action": "set", "items": [{"content": "read diff", "done": True}, {"content": "fix bug"}]}})())
        assert res.success and "2 steps, 1 done" in res.output
        agent._remind_todos()
        said = [m.content or "" for m in agent.session.context_manager._messages if m.role == "user"]
        assert any("[x] read diff" in c and "[ ] fix bug" in c for c in said)
        agent._remind_todos()  # unchanged list: not repeated
        assert sum("Task list (still current" in (m.content or "") for m in agent.session.context_manager._messages) == 1


async def test_before_tool_hook_exit_2_blocks_the_call_and_after_tool_stderr_reaches_the_model(tmp_path):
    from config.config import HookConfig, HookTrigger
    from hooks.hook_system import HookSystem

    cfg = Config(cwd=tmp_path, hooks_enabled=True, hooks=[
        HookConfig(name="nope", trigger=HookTrigger.BEFORE_TOOL, matcher="write_file", command="echo no secrets >&2; exit 2"),
        HookConfig(name="lint", trigger=HookTrigger.AFTER_TOOL, matcher="read_file", command="echo 2 lint errors >&2; exit 2"),
    ])
    hs = HookSystem(cfg)
    assert "no secrets" in await hs.trigger_before_tool("write_file", {})
    assert await hs.trigger_before_tool("read_file", {}) is None  # matcher skips other tools
    ok = ToolResult.success_result("x")
    assert "2 lint errors" in await hs.trigger_after_tool("read_file", {}, ok)


async def test_todos_complete_by_the_id_the_model_gave_in_set_or_by_a_unique_phrase(tmp_path):
    from tools.builtin.todo import TodosTool

    t = TodosTool(Config(cwd=tmp_path))
    inv = lambda **p: ToolInvocation(params=p, cwd=tmp_path)
    await t.execute(inv(action="set", items=[{"id": "inspect", "content": "Map the nav links"}, {"content": "Run lint"}]))
    assert (await t.execute(inv(action="complete", id="inspect"))).success
    assert (await t.execute(inv(action="complete", id="lint"))).success  # unique phrase of the step
    assert not (await t.execute(inv(action="complete", id="nope"))).success


async def test_graph_tool_answers_importers_and_blast_from_the_working_tree_through_ts_aliases(tmp_path):
    import subprocess

    from tools.builtin.graph import GraphTool

    (tmp_path / "src/components").mkdir(parents=True)
    (tmp_path / "src/app").mkdir()
    (tmp_path / "tsconfig.json").write_text('{ // c\n "compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"],},},}')
    (tmp_path / "src/components/Nav.tsx").write_text("export const Nav = 1\n")
    (tmp_path / "src/app/page.tsx").write_text('import { Nav } from "@/components/Nav"\n')
    (tmp_path / "src/app/page.test.tsx").write_text('import P from "./page"\n')
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    subprocess.run(["git", "add", "-A"], cwd=tmp_path, check=True)
    tool = GraphTool(Config(cwd=tmp_path))
    run = lambda **p: tool.execute(ToolInvocation(params=p, cwd=tmp_path))
    assert "src/app/page.tsx" in (await run(action="importers", name="Nav.tsx")).output
    out = (await run(action="blast", name="src/components/Nav.tsx")).output
    assert "page.tsx" in out and "page.test.tsx" in out
    assert "page.test.tsx" in (await run(action="tests", name="Nav.tsx")).output


def test_broad_requests_are_recognised():
    from agent.scope import is_broad

    for yes in ["fix all such issues in the whole app", "update every button", "rename it everywhere", "check each component for this"]:
        assert is_broad(yes), yes
    for no in ["fix the login bug in auth.ts", "add a button to the nav", "explain how the cache works"]:
        assert not is_broad(no), no


async def test_a_sweep_with_open_steps_is_sent_back_instead_of_answered(cfg):
    items = [{"id": "a", "content": "fix Nav", "done": True}, {"id": "b", "content": "fix Footer", "done": False}]
    done = [{"id": "a", "content": "fix Nav", "done": True}, {"id": "b", "content": "fix Footer", "done": True}]
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([
            call("todos", {"action": "set", "items": items}, "c1"),
            text("all fixed"),  # premature: Footer is still open
            call("todos", {"action": "set", "items": done}, "c2"),
            text("really done"),
        ])
        events = await run(agent, "fix the disabled state on all buttons in the whole app")
        sent = agent.session.client.requests
        assert len(sent) == 4
        assert "still open" in " ".join(str(m.get("content")) for m in sent[2] if m["role"] == "user") and "fix Footer" in str(sent[2])
        assert any(e.type == AgentEventType.TEXT_COMPLETE and e.data.get("content") == "really done" for e in events)


def test_interim_text_shrinks_to_one_short_line():
    from agent.brevity import interim_line

    notes = "Working notes before editing:\n\n- `a.tsx`: verified the CTA is a plain Link\n- `b.tsx:3`: state-only guard"
    assert interim_line(notes) == "Working notes before editing:"
    assert interim_line("- " + "word " * 60).endswith("…") and len(interim_line("- " + "word " * 60)) <= 142
    assert interim_line("") == ""


async def test_text_before_a_tool_call_is_shown_as_one_line_but_a_final_answer_in_full(cfg):
    (cfg.cwd / "a.txt").write_text("hi")
    notes = "Working notes:\n- one\n- two\n- three"
    async with Agent(cfg) as agent:
        sc = ScriptedClient([[*text(notes)[:-1], *call("read_file", {"path": "a.txt"})], text("Fixed it.\n- a.txt:1  done")])
        agent.session.client = sc
        events = await run(agent)
        shown = "".join(e.data.get("content", "") for e in events if e.type == AgentEventType.TEXT_DELTA)
        assert shown == "Working notes:Fixed it.\n- a.txt:1  done"
        assert any(m.get("content") == notes for m in sc.requests[1] if m["role"] == "assistant")  # history keeps the full notes
