import pytest

from config.config import Config
from tools.base import ToolInvocation
from tools.builtin.ask_user import AskUserTool


@pytest.mark.asyncio
async def test_ask_user_returns_the_picked_label_and_free_text(monkeypatch, tmp_path):
    picks = iter([1, 2])  # second option, then "something else"
    shown = []

    async def fake_choice(console, q, opts, **kw):
        shown.append((q, opts))
        return next(picks)

    async def fake_line(prompt):
        return "only CANCELLED"

    monkeypatch.setattr("ui.prompt.ask_choice", fake_choice)
    monkeypatch.setattr("ui.prompt.read_line", fake_line)
    monkeypatch.setattr("sys.stdin.isatty", lambda: True, raising=False)
    qs = [{"header": "Status", "question": f"Q{n}?", "context": "Found 2 filters in jobs.py.",
           "options": [{"label": "a", "description": "keeps it"}, {"label": "b", "description": "why"}]} for n in (1, 2)]
    res = await AskUserTool(Config(cwd=tmp_path)).execute(ToolInvocation(cwd=tmp_path, params={"questions": qs}))
    assert "Q1? -> b" in res.output and "Q2? -> only CANCELLED" in res.output
    # the user sees the topic, which question this is, the scope, and what each option does
    assert shown[0][0] == "Status (1/2): Q1?\nFound 2 filters in jobs.py."
    assert shown[0][1][:2] == ["a — keeps it", "b — why"]


def test_ask_user_requires_context_and_option_effects():
    from pydantic import ValidationError

    from tools.builtin.ask_user import AskUserParams

    with pytest.raises(ValidationError):
        AskUserParams(questions=[{"header": "X", "question": "Q?", "options": [{"label": "a"}, {"label": "b"}]}])
