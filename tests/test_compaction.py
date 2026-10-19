import re
import json

from agent.agent import Agent
from client.response import TokenUsage
from config.config import ApprovalPolicy, Config
from context.manager import ContextManager
from tests.fakes import ScriptedClient, call, text
from tools.base import ToolResult


def cm_with_history(tmp_path, n_pairs=6, big=1000):
    cm = ContextManager(Config(pi_mode=False, cwd=tmp_path), None, [])
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
    cm = ContextManager(Config(pi_mode=False, cwd=tmp_path), None, [])
    cm.add_user_message("go")
    cm.add_assistant_message("", [{"id": "a", "type": "function", "function": {"name": "x", "arguments": "{}"}},
                                  {"id": "b", "type": "function", "function": {"name": "y", "arguments": "{}"}}])
    cm.add_tool_result("a", "ok")
    assert cm.repair_orphans() == 1
    roles = [(m.role, m.tool_call_id) for m in cm._messages]
    assert roles == [("user", None), ("assistant", None), ("tool", "a"), ("tool", "b")]


async def test_agent_compacts_mid_run_and_continues(tmp_path):
    (tmp_path / "a.txt").write_text("hi")
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, compact_at_tokens=50, keep_recent_tokens=10)
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


def test_pruning_works_inside_one_task_keeps_newest_and_leaves_a_hint(tmp_path):
    cm = ContextManager(Config(pi_mode=False, cwd=tmp_path), None, [])
    cm.add_user_message("fix it")  # only one user message: a single long task
    for i in range(20):
        cm.add_assistant_message(None, [{"id": f"c{i}", "type": "function", "function": {"name": "grep", "arguments": "{}"}}])
        cm.add_tool_result(f"c{i}", f"result number {i}\n" + "word " * 3000)  # ~3k tokens each
    assert cm.prune_tool_outputs() > 0
    tools = [m for m in cm._messages if m.role == "tool"]
    assert tools[0].content.startswith("[Old tool result cleared") and "result number 0" in tools[0].content
    assert "word word" in tools[-1].content and "word word" in tools[-2].content  # newest stay whole


async def test_failed_compaction_backs_off_instead_of_retrying_every_turn(tmp_path):
    (tmp_path / "a.txt").write_text("hi")
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, compact_at_tokens=50, keep_recent_tokens=10)
    async with Agent(cfg) as a:
        tries = []

        async def failing_compress(cm):
            tries.append(1)
            return None, None, 0

        a.session.chat_compactor.compress = failing_compress
        steps = [call("read_file", {"path": "a.txt"}, f"c{i}") for i in range(3)] + [text("done")]
        a.session.client = ScriptedClient(steps)
        a.session.context_manager._latest_usage = TokenUsage(100, 10, 110)
        evs = [e async for e in a.run("go")]
        assert len(tries) == 1  # 4 turns, one failed attempt, then back-off
        assert any("pruned" in e.data.get("message", "") for e in evs if e.type.value == "notice")


def _long_history(tmp_path):
    cm = ContextManager(Config(pi_mode=False, cwd=tmp_path), None, [])
    cm.add_user_message("fix it")
    for i in range(20):
        cm.add_assistant_message(None, [{"id": f"c{i}", "type": "function", "function": {"name": "grep", "arguments": "{}"}}])
        cm.add_tool_result(f"c{i}", f"result number {i}\n" + "word " * 3000)
    return cm


def test_a_cleared_result_can_be_recalled_exactly(tmp_path):
    from utils.archive import ResultStore

    cm = _long_history(tmp_path)
    cm.archive = ResultStore(tmp_path / "res")
    original = cm._messages[2].content
    assert cm.prune_tool_outputs() > 0
    note = cm._messages[2].content
    rid = re.search(r"recall_result (r\d+)", note).group(1)
    assert cm.archive.get(rid) == original
    assert ResultStore(tmp_path / "res").get(rid) == original  # a later process can read it too


async def test_big_output_is_cut_visibly_and_the_full_text_is_recallable(tmp_path):
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, tool_output_max_chars=1000)
    async with Agent(cfg) as a:
        reg = a.session.tool_registry
        out = "line one\n" + "noise\n" * 2000 + "FAILED test_x: boom\n"
        res = ToolResult.success_result(out)
        reg._cap_output(res)
        assert res.truncated and "FAILED test_x: boom" in res.output  # the tail, where failures live, survives
        rid = re.search(r"full output: (r\d+)", res.output).group(1)
        recalled = await reg.invoke("recall_result", {"id": rid, "offset": 1, "limit": 3}, tmp_path, a.session.hook_system)
        assert recalled.output.startswith("line one\nnoise")


async def test_pruning_forgets_only_the_files_it_cleared(tmp_path):
    big = "\n".join(f"line {i} " + "word " * 20 for i in range(300))
    for n in "ab":
        (tmp_path / f"{n}.txt").write_text(big)
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, tool_output_max_chars=16_000)
    async with Agent(cfg) as agent:
        reg, cm = agent.session.tool_registry, agent.session.context_manager
        cm.PRUNE_PROTECT_TOKENS, cm.PRUNE_MINIMUM_TOKENS = 5000, 1  # room for one read: the older one is pruned

        async def read(name, cid):
            cm.add_assistant_message("", [{"id": cid, "type": "function", "function": {"name": "read_file", "arguments": json.dumps({"path": name})}}])
            res = await reg.invoke("read_file", {"path": name}, tmp_path, agent.session.hook_system, None, cid)
            cm.add_tool_result(cid, res.output)
            return res

        await read("a.txt", "c1")
        await read("b.txt", "c2")
        agent._prune(force=True)  # a.txt's text is cleared, b.txt's is still in context
        assert cm.last_prune.get("read_paths") == ["a.txt"]
        again_b = await reg.invoke("read_file", {"path": "b.txt"}, tmp_path, agent.session.hook_system, None, "c3")
        again_a = await reg.invoke("read_file", {"path": "a.txt"}, tmp_path, agent.session.hook_system, None, "c4")
        assert "already in context" in again_b.output and len(again_b.output) < 200
        assert "line 0 " in again_a.output  # cleared, so it is sent again in full


def test_cleared_result_names_the_call_that_made_it(tmp_path):
    cm = ContextManager(Config(pi_mode=True, cwd=tmp_path), None, [])
    cm.add_user_message("review it")
    for i in range(20):
        args = '{"path": "src/a%d.py"}' % i
        cm.add_assistant_message(None, [{"id": f"c{i}", "type": "function", "function": {"name": "read_file", "arguments": args}}])
        cm.add_tool_result(f"c{i}", "word " * 3000)
    assert cm.prune_tool_outputs() > 0
    first = [m for m in cm._messages if m.role == "tool"][0].content
    assert "read_file" in first and "src/a0.py" in first  # the model knows what to re-run


def test_pruning_waits_until_the_context_is_three_quarters_full(tmp_path):
    from client.response import TokenUsage

    agent = Agent.__new__(Agent)
    cfg = Config(cwd=tmp_path)
    agent.config = cfg
    cm = ContextManager(cfg, None, [])
    for i in range(8):
        cm.add_assistant_message("", [{"id": f"c{i}", "type": "function", "function": {"name": "grep", "arguments": "{}"}}])
        cm.add_tool_result(f"c{i}", "word " * 20_000)
    agent.session = type("S", (), {"context_manager": cm, "tool_registry": type("R", (), {"get": lambda self, n: type("Rd", (), {"forget": lambda self, p: None})(), "forget_instructions": lambda self: None})(), "tracer": type("T", (), {"emit": lambda *a, **k: None})()})()
    cm.set_latest_usage(TokenUsage(90_000, 10, 90_010))  # under prune_at_tokens: left alone
    agent._prune()
    assert not any(m.pruned_at for m in cm._messages)
    cm.set_latest_usage(TokenUsage(200_000, 10, 200_010))
    agent._prune()
    assert any(m.pruned_at for m in cm._messages)


def test_summarizer_sees_the_whole_user_message_and_todo_plan():
    from context.compaction import ChatCompactor
    bugs = "\n".join(f"{i}. bug number {i} " + "z" * 300 for i in range(1, 11))
    todos = json.dumps({"op": "set", "items": [f"step {i} " + "w" * 150 for i in range(12)]})
    text = ChatCompactor(None)._format_history_for_compaction([
        {"role": "user", "content": bugs},
        {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "todos", "arguments": todos}}]},
    ])
    assert "10. bug number 10" in text  # was cut at 1500 chars, losing items 6-10
    assert "step 11" in text  # was cut at 500 chars, losing the plan after step 4


def test_compacted_messages_are_saved_and_the_summary_points_at_them(tmp_path):
    cm = cm_with_history(tmp_path)
    log = tmp_path / "t" / "s.jsonl"
    cut = cm.find_cut(600)
    cm.apply_compaction("SUMMARY", cut, log)
    saved = [json.loads(line) for line in log.read_text().splitlines()]
    assert saved[0]["content"].startswith("task 0") and len(saved) == cut
    assert str(log) in cm._messages[0].content
    # a resumed session still pulls the summary back out with the pointer in the intro
    cm2 = ContextManager(Config(pi_mode=False, cwd=tmp_path), None, [])
    cm2.add_user_message(cm._messages[0].content)
    assert cm2.summary.startswith("SUMMARY")
    # the second compaction appends only new messages, not the summary head
    cm.add_user_message("more " + "m" * 2000)
    cm.apply_compaction("SECOND", cm.find_cut(300), log)
    assert not any(json.loads(l)["content"].startswith("# Earlier") for l in log.read_text().splitlines())


async def test_folder_rules_come_back_after_a_compaction(tmp_path):
    (tmp_path / "web").mkdir()
    (tmp_path / "web" / "AGENTS.md").write_text("WEB RULE: ask before restyling")
    (tmp_path / "web" / "a.ts").write_text("x")
    (tmp_path / "web" / "b.ts").write_text("y")
    cfg = Config(pi_mode=False, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, keep_recent_tokens=10)
    async with Agent(cfg) as a:
        async def fake_compress(cm):
            return "FOLDED", TokenUsage(1, 1, 2), len(cm._messages)

        a.session.chat_compactor.compress = fake_compress
        a.session.client = ScriptedClient([call("read_file", {"path": "web/a.ts"}), text("done")])
        [e async for e in a.run("first")]
        assert await a._compact()  # the read that carried the rule is folded into the summary
        a.session.client = ScriptedClient([call("read_file", {"path": "web/b.ts"}, "c2"), text("done")])
        [e async for e in a.run("second")]
        results = [m.content for m in a.session.context_manager._messages if m.role == "tool"]
        assert "WEB RULE" in results[-1]


async def test_files_worked_in_come_back_after_a_compaction_as_they_are_on_disk(tmp_path):
    (tmp_path / "a.ts").write_text("old text")
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, keep_recent_tokens=10)
    async with Agent(cfg) as a:
        async def fake_compress(cm):
            return "FOLDED", TokenUsage(1, 1, 2), len(cm._messages)

        a.session.chat_compactor.compress = fake_compress
        a.session.client = ScriptedClient([call("read_file", {"path": "a.ts"}), text("done")])
        [e async for e in a.run("first")]
        (tmp_path / "a.ts").write_text("NEW TEXT")  # edited since it was read
        assert await a._compact()
        last = a.session.context_manager._messages[-1].content
        assert "## a.ts\nNEW TEXT" in last and "old text" not in last


def test_recent_files_are_newest_first_and_skip_what_the_tail_still_has(tmp_path):
    cm = cm_with_history(tmp_path, n_pairs=4)  # reads f0..f3, four messages per pair
    assert cm.recent_files(12) == ["f2.py", "f1.py", "f0.py"]  # f3 sits in the kept tail
    assert cm.recent_files(16, limit=2) == ["f3.py", "f2.py"]
