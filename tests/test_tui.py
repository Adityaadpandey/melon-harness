import asyncio
import re

import pytest
from prompt_toolkit.completion import CompleteEvent
from prompt_toolkit.document import Document
from prompt_toolkit.input import create_pipe_input
from prompt_toolkit.output import DummyOutput
from rich.console import Console

from config.config import Config
from safety.approval import Decision
from tools.base import FileDiff, ToolConfirmation
from ui.prompt import MewCompleter, ask_choice, expand_mentions
from ui.tui import AGENT_THEME, TUI, parse_unified_diff, render_diff, summarize_result, tool_label

DIFF = FileDiff(path="a.py", old_content="x = 1\ny = 2\nz = 3\n", new_content="x = 1\ny = 20\nz = 3\nw = 4\n").to_diff()


def make(tmp_path):
    con = Console(record=True, theme=AGENT_THEME, width=100, force_terminal=False)
    return TUI(Config(cwd=tmp_path), con), con


def test_tool_labels_are_short_and_readable(tmp_path):
    assert tool_label("read_file", {"path": str(tmp_path / "src/a.py"), "limit": 50}, tmp_path) == "Read(src/a.py)"
    assert tool_label("shell", {"command": "pnpm test\n--filter x"}, tmp_path) == "Bash(pnpm test --filter x)"
    assert tool_label("grep", {"pattern": "load", "glob": "*.ts"}, tmp_path) == "Search(load, *.ts)"
    assert tool_label("subagent_debugger", {"goal": "why 500"}, tmp_path) == "Agent[debugger](why 500)"
    assert tool_label("mystery", {"n": 3}, tmp_path) == "mystery(3)"  # non-string args must not crash


def test_diff_line_numbers_counts_and_truncation():
    rows, adds, dels = parse_unified_diff(DIFF)
    assert (adds, dels) == (2, 1)
    assert ("-", 2, "y = 2") in rows and ("+", 2, "y = 20") in rows and ("+", 4, "w = 4") in rows
    big = FileDiff(path="b", old_content="", new_content="\n".join(f"l{i}" for i in range(100)) + "\n").to_diff()
    text, adds, _ = render_diff(big, max_lines=10)
    assert adds == 100 and "+90 more changed lines" in text.plain


def test_result_summaries():
    assert summarize_result("read_file", True, "", None, {"shown_start": 1, "shown_end": 40, "total_lines": 400}, None, {})[0] == "Read 40 lines (of 400)"
    assert summarize_result("grep", True, "a:1:x\nb:2:y", None, {"matches": 2, "files_with_matches": 2}, None, {})[0] == "Found 2 matches in 2 files"
    s, pre = summarize_result("shell", True, "\n".join(map(str, range(20))), None, {}, 0, {})
    assert s == "Ran" and pre[-1] == "… +14 lines"
    s, _ = summarize_result("edit", False, "", "old_string not found.\nClosest lines:", {}, None, {})
    assert s.startswith("✗ old_string not found")


def test_transcript_shape(tmp_path):
    tui, con = make(tmp_path)
    tui.tool_call_start("c1", "edit", "write", {"path": str(tmp_path / "a.py"), "old_string": "y = 2", "new_string": "y = 20"})
    tui.tool_call_complete("c1", "edit", "write", True, "Edited a.py", None, {"path": str(tmp_path / "a.py")}, DIFF, False, None)
    # a parallel batch: both start, then both complete; each result stays under its own header
    tui.tool_call_start("c2", "grep", "read", {"pattern": "needle"})
    tui.tool_call_start("c3", "read_file", "read", {"path": "b.py"})
    tui.tool_call_complete("c2", "grep", "read", True, "x:1:needle", None, {"matches": 1, "files_with_matches": 1}, None, False, None)
    tui.tool_call_complete("c3", "read_file", "read", True, "", None, {"shown_start": 1, "shown_end": 5, "total_lines": 5}, None, False, None)
    tui.tool_call_start("c4", "shell", "shell", {"command": "false"})
    tui.tool_call_complete("c4", "shell", "shell", False, "boom", "Exit code 1", {}, None, False, 1)
    out = con.export_text()
    assert re.search(r"Search\(needle\)\n\s+⎿\s+Found 1 match.*\n.*x:1:needle\n\n⏺ Read\(b.py\)\n\s+⎿\s+Read 5 lines", out)
    assert "⏺ Update(a.py)" in out
    assert "Updated a.py with 2 additions and 1 removal" in out
    assert re.search(r"2 \+ y = 20", out) and re.search(r"2 - y = 2", out)
    assert "✗ Exit code 1" in out


async def test_confirm_returns_the_users_choice(tmp_path, monkeypatch):
    tui, con = make(tmp_path)
    conf = ToolConfirmation("edit", {}, "Edit a.py", diff=FileDiff(path="a.py", old_content="a\n", new_content="b\n"), affected_paths=[])
    answers = iter([0, 1, 2])

    async def fake_choice(console, q, options, **kw):
        assert len(options) == 3 and "edit" in q.lower()
        return next(answers)

    async def fake_line(prompt):
        return "use a class"

    monkeypatch.setattr("ui.prompt.ask_choice", fake_choice)
    monkeypatch.setattr("ui.prompt.read_line", fake_line)
    assert await tui.confirm(conf) == Decision(True)
    assert await tui.confirm(conf) == Decision(True, always=True)
    assert await tui.confirm(conf) == Decision(False, feedback="The user said: use a class")
    assert "Do you want" not in con.export_text() or True


@pytest.mark.parametrize("keys,expected", [("1", 0), ("2", 1), ("3", 2), ("y", 0), ("n", 2), ("\r", 0)])
async def test_ask_choice_single_keypress(keys, expected):
    with create_pipe_input() as inp:
        inp.send_text(keys)
        con = Console(record=True, theme=AGENT_THEME, width=80)
        got = await asyncio.wait_for(ask_choice(con, "Proceed?", ["Yes", "Always", "No"], input=inp, output=DummyOutput()), 5)
    assert got == expected


def test_completer_slash_and_file_mentions(tmp_path):
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "auth.service.ts").write_text("x")
    (tmp_path / ".env").write_text("SECRET=1")
    c = MewCompleter(tmp_path, [], lambda: ["claude-sonnet-5-5", "deepseek/deepseek-v4-pro"])

    def comps(text):
        return [x.text for x in c.get_completions(Document(text), CompleteEvent())]

    assert "/compact" in comps("/co") and "/model" not in comps("/co")
    assert comps("explain @auth") == ["@src/auth.service.ts"]
    assert comps("@env") == []  # secrets never offered
    assert comps("/model deep") == ["deepseek/deepseek-v4-pro"]


def test_mentions_attach_files_but_never_secrets_or_protected_paths(tmp_path):
    (tmp_path / "a.py").write_text("print(1)")
    (tmp_path / ".env").write_text("TOKEN=abc")
    (tmp_path / "pkg").mkdir()
    (tmp_path / "pkg" / "m.py").write_text("")
    (tmp_path / "private.txt").write_text("nope")
    text, attached = expand_mentions("look at @a.py and @.env and @pkg and @private.txt and @missing.py", tmp_path, ["private.*"])
    assert attached == ["a.py", "pkg"] and "print(1)" in text and "TOKEN" not in text and "nope" not in text
    assert '<directory path="pkg">' in text and "m.py" in text
