import pytest

from config.config import Config
from tools.base import ToolInvocation
from tools.builtin.ask_user import AskUserTool


@pytest.mark.asyncio
async def test_ask_user_returns_the_picked_label_and_free_text(monkeypatch, tmp_path):
    picks = iter([1, 2])  # second option, then "something else"

    async def fake_choice(console, q, opts, **kw):
        return next(picks)

    async def fake_line(prompt):
        return "only CANCELLED"

    monkeypatch.setattr("ui.prompt.ask_choice", fake_choice)
    monkeypatch.setattr("ui.prompt.read_line", fake_line)
    monkeypatch.setattr("sys.stdin.isatty", lambda: True, raising=False)
    qs = [{"question": f"Q{n}?", "options": [{"label": "a"}, {"label": "b", "description": "why"}]} for n in (1, 2)]
    res = await AskUserTool(Config(cwd=tmp_path)).execute(ToolInvocation(cwd=tmp_path, params={"questions": qs}))
    assert "Q1? -> b" in res.output and "Q2? -> only CANCELLED" in res.output
