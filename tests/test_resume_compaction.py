"""Compact a run on a small repo, save it, resume it, compact again: nothing from before the
first compaction may be lost along the way."""
import json

from client.llm_client import LLMClient
from context.compaction import ChatCompactor
from context.manager import ContextManager
from config.config import Config


def add_pair(cm, i, name, path):
    tid = f"t{i}"
    cm.add_user_message(f"step {i} " + "x" * 800)
    cm.add_assistant_message("", [{"id": tid, "type": "function", "function": {"name": name, "arguments": json.dumps({"path": path})}}])
    cm.add_tool_result(tid, "ok " + "y" * 800)
    cm.add_assistant_message(f"done {i}")


def resume(cfg, messages):
    """Same replay main._resume does from a saved snapshot."""
    cm = ContextManager(cfg, None, [])
    for m in messages:
        if m["role"] == "user":
            cm.add_user_message(m["content"])
        elif m["role"] == "assistant":
            cm.add_assistant_message(m.get("content", ""), m.get("tool_calls"))
        elif m["role"] == "tool":
            cm.add_tool_result(m["tool_call_id"], m["content"])
    return cm


def test_second_compaction_after_resume_keeps_the_first_summary_and_edited_files(tmp_path):
    cfg = Config(cwd=tmp_path, keep_recent_tokens=600)
    cm = ContextManager(cfg, None, [])
    add_pair(cm, 0, "edit", "app/main.py")
    for i in range(1, 6):
        add_pair(cm, i, "read_file", f"app/f{i}.py")
    long_summary = "## ORIGINAL GOAL\nFix the login bug in app/main.py. " + "detail " * 400 + "\nMARKER-END"
    cm.apply_compaction(long_summary, cm.find_cut(600))

    resumed = resume(cfg, cm.get_messages())
    for i in range(6, 10):
        add_pair(resumed, i, "read_file", f"app/g{i}.py")

    seen = {}

    class Spy:
        async def chat_completion(self, messages, stream=False, role=""):
            seen["prompt"] = messages[-1]["content"]
            return
            yield

    asyncio_run(ChatCompactor(Spy()).compress(resumed))
    prompt = seen["prompt"]
    assert "MARKER-END" in prompt, "the first summary was cut short when folded into the second"
    assert prompt.count("MARKER-END") == 1, "the summary should be passed once, not also as a message"
    assert "app/main.py" in prompt
    assert "Files modified so far: app/main.py" in prompt


def asyncio_run(coro):
    import asyncio

    return asyncio.run(coro)
