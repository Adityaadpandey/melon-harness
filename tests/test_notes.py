import asyncio

from agent import notes
from tools.base import ToolInvocation
from tools.builtin.notes import NotesTool
from config.config import Config


def test_notes_roundtrip_and_prompt(tmp_path, monkeypatch):
    monkeypatch.setattr("config.loader.get_data_dir", lambda: tmp_path / "data")
    work = tmp_path / "repo"
    work.mkdir()
    notes.write(work, "Goal: move pages\nNext: run check-types")
    assert "check-types" in notes.read(work)
    assert "Task notes" in notes.prompt_section(work) and "Goal: move pages" in notes.prompt_section(work)
    notes.write(work, "")
    assert notes.read(work) == "" and notes.prompt_section(work) == ""


def test_stale_notes_are_not_shown(tmp_path, monkeypatch):
    monkeypatch.setattr("config.loader.get_data_dir", lambda: tmp_path / "data")
    notes.write(tmp_path, "old plan")
    monkeypatch.setattr(notes, "age_days", lambda *a, **k: 30.0)
    assert notes.prompt_section(tmp_path) == ""


def test_tool_actions(tmp_path, monkeypatch):
    monkeypatch.setattr("config.loader.get_data_dir", lambda: tmp_path / "data")
    tool = NotesTool(Config(cwd=tmp_path))

    def call(**p):
        return asyncio.run(tool.execute(ToolInvocation(params=p, cwd=tmp_path)))

    assert call(action="write", text="Done: a").success
    assert "Done: a" in call(action="read").output
    assert not call(action="write").success
    call(action="clear")
    assert "no notes" in call(action="read").output
