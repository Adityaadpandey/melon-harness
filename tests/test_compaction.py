import json

from agent.agent import Agent
from client.response import TokenUsage
from config.config import ApprovalPolicy, Config
from context.manager import ContextManager
from tests.fakes import ScriptedClient, call, text


def cm_with_history(tmp_path, n_pairs=6, big=1000):
    cm = ContextManager(Config(cwd=tmp_path), None, [])
    for i in range(n_pairs):
        cm.add_user_message(f"task {i} " + "x" * big)
        tid = f"t{i}"
        cm.add_assistant_message("", [{"id": tid, "type": "function", "function": {"name": "read_file", "arguments": json.dumps({"path": f"f{i}.py"})}}])
        cm.add_tool_result(tid, "content " + "y" * big)
        cm.add_assistant_message(f"done {i}")
    return cm


def test_cut_never_splits_a_tool_call_from_its_result(tmp_path):
    cm = cm_with_history(tmp_path)
    for keep in (1, 300, 900, 1500, 5000):
        cut = cm.find_cut(keep)
        assert cut == len(cm._messages) or cm._messages[cut].role != "tool"
    assert cm.find_cut(10**9) == 0


def test_compaction_keeps_the_recent_tail_verbatim_and_tracks_files(tmp_path):
    cm = cm_with_history(tmp_path)
    tail_before = [m.content for m in cm._messages[-4:]]
    cut = cm.find_cut(600)
    assert 0 < cut < len(cm._messages)
    cm.apply_compaction("SUMMARY TEXT", cut)
    assert cm._messages[0].role == "user" and "SUMMARY TEXT" in cm._messages[0].content
    assert [m.content for m in cm._messages[-4:]] == tail_before
    assert "f0.py" in cm.summary  # files read are listed deterministically, not left to the model
    # a second compaction folds into the running summary and keeps accumulating files
    cm.add_user_message("more")
    cut2 = cm.find_cut(300)
    cm.apply_compaction("SECOND", cut2)
    assert cm.summary.startswith("SECOND") and "f0.py" in cm.summary


def test_repair_orphans_closes_unanswered_tool_calls(tmp_path):
    cm = ContextManager(Config(cwd=tmp_path), None, [])
    cm.add_user_message("go")
    cm.add_assistant_message("", [{"id": "a", "type": "function", "function": {"name": "x", "arguments": "{}"}},
                                  {"id": "b", "type": "function", "function": {"name": "y", "arguments": "{}"}}])
    cm.add_tool_result("a", "ok")
    assert cm.repair_orphans() == 1
    roles = [(m.role, m.tool_call_id) for m in cm._messages]
    assert roles == [("user", None), ("assistant", None), ("tool", "a"), ("tool", "b")]


async def test_agent_compacts_mid_run_and_continues(tmp_path):
    (tmp_path / "a.txt").write_text("hi")
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, compact_at_tokens=50, keep_recent_tokens=10)
    async with Agent(cfg) as a:
        seen = {}

        async def fake_compress(cm):
            seen["called"] = True
            return "FOLDED", TokenUsage(1, 1, 2), cm.find_cut(10)

        a.session.chat_compactor.compress = fake_compress
        a.session.client = ScriptedClient([call("read_file", {"path": "a.txt"}), call("read_file", {"path": "a.txt"}, "c2"), text("done")])
        a.session.context_manager._latest_usage = TokenUsage(100, 10, 110)  # already over the 50-token limit
        evs = [e async for e in a.run("go")]
        assert seen.get("called")
        assert any("compacted" in e.data.get("message", "") for e in evs if e.type.value == "notice")
