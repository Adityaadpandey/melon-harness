import asyncio

from agent import review_pipeline as rp
from config.config import Config


async def test_a_stalled_sub_agent_is_cut_off_and_answers_empty(monkeypatch):
    class Stuck:
        def __init__(self, cfg):
            self.cfg = cfg

        async def __aenter__(self):
            await asyncio.sleep(3600)

        async def __aexit__(self, *a):
            pass

    import agent.agent as agent_mod
    monkeypatch.setattr(agent_mod, "Agent", Stuck)
    real = asyncio.wait_for
    monkeypatch.setattr(asyncio, "wait_for", lambda coro, timeout: real(coro, 0.2))
    assert await rp.ask(Config(), "review this", "default", 3, 1000) == ""


def test_sub_agents_are_marked_so_their_answer_is_not_rewritten():
    assert Config().sub_agent is False
