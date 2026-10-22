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
        async def chat_completion(self, messages, tools=None, stream=False, role=""):
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


def test_compaction_that_overflows_drops_the_oldest_messages_and_asks_again(tmp_path):
    from client.response import StreamEvent, StreamEventType, TextDelta, TokenUsage

    cfg = Config(cwd=tmp_path, keep_recent_tokens=600)
    cm = ContextManager(cfg, None, [])
    for i in range(10):
        add_pair(cm, i, "read_file", f"app/f{i}.py")
    sizes = []

    class TooLongOnce:
        async def chat_completion(self, messages, tools=None, stream=False, role=""):
            sizes.append(len(messages[-1]["content"]))
            if len(sizes) == 1:
                yield StreamEvent(type=StreamEventType.ERROR, error="prompt is too long")
            else:
                yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, text_delta=TextDelta(GOOD), usage=TokenUsage(1, 1, 2))

    summary, _, cut = asyncio_run(ChatCompactor(TooLongOnce()).compress(cm))
    assert summary == GOOD and cut > 0 and sizes[1] < sizes[0]


GOOD = "## ORIGINAL GOAL\nfix paging\n## COMPLETED ACTIONS\nread f1\n## NEXT STEP\nedit f2"


def test_a_summary_cut_off_or_without_its_sections_is_not_saved(tmp_path):
    from client.response import StreamEvent, StreamEventType, TextDelta

    cfg = Config(cwd=tmp_path, keep_recent_tokens=600)
    cm = ContextManager(cfg, None, [])
    for i in range(10):
        add_pair(cm, i, "read_file", f"app/f{i}.py")
    replies = [(GOOD, "length"), ("Sure! The user wants paging fixed.", "stop"), (GOOD, "stop")]

    class Summarizer:
        async def chat_completion(self, messages, tools=None, stream=False, role=""):
            text, finish = replies.pop(0)
            yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, text_delta=TextDelta(text), finish_reason=finish)

    summary, _, _ = asyncio_run(ChatCompactor(Summarizer()).compress(cm))
    assert summary == GOOD and replies == []  # the half summary and the chatty one were both asked again


def test_resumed_archive_keeps_old_ids_and_never_overwrites_them(tmp_path):
    from utils.archive import ResultStore
    old = ResultStore(tmp_path / "s1")
    old.put("first"), old.put("second")
    fresh = ResultStore(tmp_path / "throwaway")
    fresh.move_to(tmp_path / "s1")
    assert fresh.get("r2") == "second"  # a cleared result from before the resume is still recallable
    assert fresh.put("third") == "r3" and fresh.get("r1") == "first"


def test_the_summary_is_asked_on_the_cached_prefix_and_falls_back_to_the_trimmed_copy(tmp_path):
    from client.response import StreamEvent, StreamEventType, TextDelta
    from context.compaction import FORK_PROMPT

    cfg = Config(cwd=tmp_path, keep_recent_tokens=600)
    cm = ContextManager(cfg, None, [])
    for i in range(10):
        add_pair(cm, i, "read_file", f"app/f{i}.py")
    asked = []

    class Summarizer:
        def __init__(self, replies):
            self.replies = replies

        async def chat_completion(self, messages, tools=None, stream=False, role=""):
            asked.append((messages, tools, role))
            yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, text_delta=TextDelta(self.replies.pop(0)))

    tools = [{"name": "read_file"}]
    summary, _, cut = asyncio_run(ChatCompactor(Summarizer([GOOD])).compress(cm, fork=(tools, "strong")))
    sent, sent_tools, role = asked[0]
    assert summary == GOOD and sent[:-1] == cm.get_messages()[: cut + 1]  # byte-for-byte the prefix the loop sent
    assert sent[-1]["content"] == FORK_PROMPT and sent_tools == tools and role == "strong"

    asked.clear()
    summary, _, _ = asyncio_run(ChatCompactor(Summarizer(["no headings here", GOOD])).compress(cm, fork=(tools, "strong")))
    assert summary == GOOD and asked[1][2] == "summarize"  # the fork failed, the trimmed copy did the job
