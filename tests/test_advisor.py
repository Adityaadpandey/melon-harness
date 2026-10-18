from pathlib import Path
from types import SimpleNamespace

from client.response import StreamEvent, StreamEventType
from config.config import Config
from tools.base import ToolInvocation
from tools.builtin.advisor import AdvisorTool, render_transcript
from tools.registry import create_default_registry


class FakeClient:
    def __init__(self, reply="1. the cause is the shared helper", error=""):
        self.reply, self.error, self.seen = reply, error, []

    async def chat_completion(self, messages, tools=None, stream=True, role="default"):
        self.seen.append((role, messages))
        if self.error:
            yield StreamEvent(type=StreamEventType.ERROR, error=self.error)
            return
        yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, text_delta=SimpleNamespace(content=self.reply))


def call(tool):
    import asyncio
    return asyncio.run(tool.execute(ToolInvocation(params={}, cwd=Path("."))))


def test_the_transcript_keeps_the_request_and_drops_the_oldest_middle_when_it_is_too_long():
    msgs = [{"role": "system", "content": "sys"}, {"role": "user", "content": "fix the login bug"}]
    msgs += [{"role": "tool", "content": f"result {n} " + "x" * 1000} for n in range(100)]
    text = render_transcript(msgs, cap=5000)
    assert "fix the login bug" in text and "sys" not in text and "[... earlier steps left out ...]" in text and "result 99" in text and "result 0 " not in text


def test_the_advisor_forwards_the_whole_conversation_to_the_advisor_role_and_returns_its_advice(tmp_path):
    client = FakeClient()
    tool = AdvisorTool(Config(cwd=tmp_path), client, lambda: [{"role": "user", "content": "task"}, {"role": "assistant", "content": "plan", "tool_calls": [{"function": {"name": "edit", "arguments": "{}"}}]}])
    out = call(tool)
    assert out.success and out.output.startswith("ADVISOR:") and "shared helper" in out.output
    role, messages = client.seen[0]
    assert role == "advisor" and "[user] task" in messages[1]["content"] and "-> edit(" in messages[1]["content"]


def test_the_advisor_never_blocks_the_work_when_it_cannot_answer_and_stops_at_its_call_limit(tmp_path):
    assert "not connected" in call(AdvisorTool(Config(cwd=tmp_path))).error
    bad = AdvisorTool(Config(cwd=tmp_path), FakeClient(error="quota"), lambda: [])
    assert "could not answer" in call(bad).error and "Carry on" in call(bad).error
    ok = AdvisorTool(Config(cwd=tmp_path, advisor_max_calls=1), FakeClient(), lambda: [])
    assert call(ok).success and "limit reached" in call(ok).error


def test_the_advisor_is_a_default_tool_but_a_sub_agent_does_not_get_it(tmp_path):
    assert "advisor" in Config(cwd=tmp_path).allowed_tools and "advisor" not in Config(cwd=tmp_path, model_role="review").allowed_tools
    assert create_default_registry(Config(cwd=tmp_path)).get("advisor") is not None
    assert "advisor" not in Config(cwd=tmp_path, advisor=False).allowed_tools


async def test_a_gate_refuses_a_call_with_its_message_and_lets_everything_else_through(tmp_path):
    from safety.approval import ApprovalManager
    from hooks.hook_system import HookSystem
    reg = create_default_registry(Config(cwd=tmp_path))
    (tmp_path / "a.txt").write_text("x\n")
    reg.gate = lambda name: "Consult the advisor first." if name == "edit" else None
    cfg = Config(cwd=tmp_path)
    blocked = await reg.invoke("edit", {"path": "a.txt", "old_string": "x", "new_string": "y"}, tmp_path, HookSystem(cfg), ApprovalManager(cfg.approval, tmp_path))
    assert not blocked.success and "Consult the advisor first." in blocked.error and (tmp_path / "a.txt").read_text() == "x\n"
