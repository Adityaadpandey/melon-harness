"""A final answer far over the limit is not rewritten (the user already read it), but the next request carries a reminder."""
from agent.brevity import LONG_ANSWER_WORDS
from agent.review import ReviewResult


def test_limit_is_well_above_the_prompt_rule():
    assert LONG_ANSWER_WORDS > 120  # the prompt asks for 120; only a clear overrun triggers the reminder


def test_review_result_counts_blocked_calls():
    r = ReviewResult("PASS", "")
    assert r.blocked == 0
    r.blocked = 2
    assert r.blocked == 2


import subprocess

from agent.agent import Agent
from agent.events import AgentEventType
from config.config import ApprovalPolicy, Config, HookConfig, HookTrigger
from tests.fakes import ScriptedClient, text


def _repo(tmp_path):
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True, capture_output=True)
    return tmp_path


async def test_long_answer_earns_a_reminder_next_request(tmp_path):
    cfg = Config(pi_mode=False, cwd=_repo(tmp_path), approval=ApprovalPolicy.AUTO, auto_verify=False, check_references=False)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([text("word " * 400), text("ok")])
        _ = [e async for e in a.run("explain it")]
        _ = [e async for e in a.run("and again")]
        assert "ran 400 words" in a.session.client.requests[1][-1]["content"]


async def test_stop_hook_sends_the_work_back_once_then_lets_go(tmp_path):
    hook = HookConfig(name="tests", trigger=HookTrigger.STOP, command="echo 2 tests fail >&2; exit 2")
    cfg = Config(pi_mode=False, cwd=_repo(tmp_path), approval=ApprovalPolicy.AUTO, auto_verify=False, check_references=False, hooks_enabled=True, hooks=[hook])
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([text("done"), text("done again"), text("final")])
        evs = [e async for e in a.run("hi")]
        assert sum(1 for e in evs if "stop hook" in e.data.get("message", "")) == 2  # capped at two
        assert any("2 tests fail" in str(m.get("content")) for m in a.session.client.requests[1])
