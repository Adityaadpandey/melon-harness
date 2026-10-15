from agent.routing_policy import EFFORT, classify, length_hint
from client.llm_client import LLMClient
from config.config import Config


def test_difficulty_sets_effort_and_length():
    assert EFFORT[classify("rename foo to bar in utils.py")[0]] == "low"
    assert EFFORT[classify("debug the flaky webhook retry")[0]] == "high"
    assert length_hint("review my change", "default", False).startswith("Answer with at most 6 findings")
    assert "300 words" in length_hint("explain how the router works", "default", False)
    assert "1-2 sentences" in length_hint("rename foo to bar", "fast", True)
    assert length_hint("debug the race in the queue", "strong", True) == ""


async def test_summaries_think_low_and_an_explicit_effort_wins(tmp_path, monkeypatch):
    seen = []

    class Codex:
        _effort = None

        async def stream(self, *a):
            seen.append(self._effort)
            return
            yield

    async def run(cfg_effort, role):
        c = LLMClient(Config(cwd=tmp_path, reasoning_effort=cfg_effort))
        c.auto_effort = "high"
        monkeypatch.setattr(c, "_adapter", lambda endpoint: Codex())
        monkeypatch.setattr(c.registry, "get", lambda m: type("I", (), {"endpoint": "/responses"})())
        async for _ in c._call_model("codex/x", role, [], None, True, type("C", (), {"emitted": False})()):
            pass

    await run(None, "default")
    await run("low", "default")
    await run("high", "summarize")
    assert seen == ["high", "low", "low"]
