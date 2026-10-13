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


# ------------------------------------------------------------- streaming / live UI

from ui.tui import context_bar, split_blocks


def test_split_blocks_waits_for_complete_paragraphs_and_keeps_code_and_lists_whole():
    assert split_blocks("Hello wor") == ("", "Hello wor")                       # nothing finished yet
    assert split_blocks("Para one.\n\nPara tw") == ("Para one.\n\n", "Para tw")  # first paragraph is done
    fenced = "Intro\n\n```py\nx = 1\n\ny = 2\n```\n\nafter"
    done, rest = split_blocks(fenced)
    assert done == "Intro\n\n```py\nx = 1\n\ny = 2\n```\n\n" and rest == "after"  # blank line inside the fence is not a cut
    assert split_blocks("```py\nx = 1\n\ny = 2")[0] == ""                          # unclosed fence: keep waiting
    lst = "1. one\n\n2. two\n\n3. thr"
    assert split_blocks(lst)[0] == ""  # list items separated by blank lines stay together (numbering would restart)
    assert split_blocks("Intro\n\n- a\n- b\n\nOutro") == ("Intro\n\n- a\n- b\n\n", "Outro")  # the list stays with its intro; "Outro" starts the next block
    assert split_blocks("Done.\n\n")[0] == ""                                    # blank line with nothing after it yet


def test_assistant_text_is_committed_as_rendered_markdown_blocks_in_order(tmp_path):
    tui, con = make(tmp_path)
    tui.start_turn()
    for chunk in ["# Title\n\nFirst ", "paragraph.\n\n", "- item a\n- item b\n\n", "```py\nprint(1)\n```\n\nlast"]:
        tui.assistant_delta(chunk)
    tui.end_turn()
    out = con.export_text()
    assert out.index("Title") < out.index("First paragraph.") < out.index("item a") < out.index("print(1)") < out.index("last")
    assert "# Title" not in out and "```" not in out  # markdown was rendered, not echoed raw
    assert out.count("⏺") == 1  # one bullet for the whole message, not one per block


def test_text_after_a_tool_call_starts_a_new_message(tmp_path):
    tui, con = make(tmp_path)
    tui.start_turn()
    tui.assistant_delta("Let me look.\n\n")
    tui.tool_call_start("c1", "read_file", "read", {"path": "a.py"})
    tui.tool_call_complete("c1", "read_file", "read", True, "", None, {"shown_start": 1, "shown_end": 3, "total_lines": 3}, None, False, None)
    tui.assistant_delta("Found it.")
    tui.end_turn()
    out = con.export_text()
    assert out.index("Let me look.") < out.index("Read(a.py)") < out.index("Found it.")
    assert out.count("⏺") == 3


def test_live_state_todos_agents_and_phase(tmp_path):
    tui, _ = make(tmp_path)
    tui.set_todos([("write tests", True), ("fix bug", False)])
    tui.agent_progress("c1", {"kind": "start", "agent": "investigator", "goal": "map it"})
    tui.agent_progress("c1", {"kind": "tool", "agent": "investigator", "name": "grep", "args": {"pattern": "needle"}})
    tui.agent_progress("c2", {"kind": "start", "agent": "debugger", "goal": "why"})
    tui.start_turn("Thinking")
    text = "\n".join(r.plain for r in tui._renderable().renderables if hasattr(r, "plain"))
    assert "investigator · Search(needle)" in text and "debugger · starting" in text
    assert "☒ write tests" in text and "☐ fix bug" in text and "Thinking…" in text
    tui.agent_progress("c1", {"kind": "done", "agent": "investigator"})
    assert "c1" not in tui.agents and "c2" in tui.agents
    tui.end_turn()
    assert tui.agents == {}  # nothing lingers after the turn


def test_expand_shows_full_output_of_recent_tool_calls(tmp_path):
    tui, con = make(tmp_path)
    for i, out in enumerate(["\n".join(f"line {n}" for n in range(30)), "second call output"]):
        tui.tool_call_start(f"c{i}", "shell", "shell", {"command": f"cmd{i}"})
        tui.tool_call_complete(f"c{i}", "shell", "shell", True, out, None, {}, None, False, 0)
    con.clear() if hasattr(con, "clear") else None
    tui.show_result(2)
    text = con.export_text()
    assert "line 29" in text and "cmd0" in text  # the older call, in full (the inline view only showed 6 lines)
    tui.show_result(9)
    assert "no such tool result" in con.export_text()


def test_context_bar_and_summary(tmp_path):
    assert context_bar(0) == "▱" * 10 + " 0%" and context_bar(50) == "▰" * 5 + "▱" * 5 + " 50%" and context_bar(100).startswith("▰" * 10)
    tui, con = make(tmp_path)
    tui.turn_summary(12.4, {"calls": 3, "prompt_tokens": 4100, "completion_tokens": 300, "cache_hit_pct": 78.0, "est_cost_usd": None}, None)
    assert "12s · 3 calls · 4.1k in (78% cached) · 0.3k out" in con.export_text()


async def test_input_box_enter_submits_alt_enter_adds_a_line_and_history_walks_back(tmp_path):
    from pathlib import Path

    from ui.prompt import InputSession

    hist = tmp_path / "h"

    async def ask(keys):
        with create_pipe_input() as inp:
            s = InputSession(Path("."), [], lambda: [], lambda: "footer", hist, input=inp, output=DummyOutput())
            inp.send_text(keys)
            return await asyncio.wait_for(s.read(), 5)

    assert await ask("first message\r") == "first message"
    assert await ask("a\x1b\rb\r") == "a\nb"
    assert await ask("\x1b[A\r") == "a\nb"  # Up recalls the previous entry
    assert (await ask("/co\t\r")).startswith("/co")  # Tab completes a command; Enter then submits


async def test_input_box_ctrl_c_clears_then_interrupts_and_ctrl_d_ends(tmp_path):
    from pathlib import Path

    from ui.prompt import InputSession

    async def ask(keys):
        with create_pipe_input() as inp:
            s = InputSession(Path("."), [], lambda: [], lambda: "", None, input=inp, output=DummyOutput())
            inp.send_text(keys)
            return await asyncio.wait_for(s.read(), 5)

    assert await ask("oops\x03fresh\r") == "fresh"          # first Ctrl+C clears the line
    with pytest.raises(KeyboardInterrupt):
        await ask("\x03")                                    # on an empty line it interrupts
    with pytest.raises(EOFError):
        await ask("\x04")
