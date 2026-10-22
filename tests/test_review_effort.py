from agent import review_pipeline as rp


def test_small_change_is_light_and_a_big_or_risky_one_is_not():
    assert rp.is_light("+x\n" * 16, ["deploy.yml"], False)
    assert not rp.is_light("+x\n" * 200, ["a.ts"], False)
    assert not rp.is_light("+x\n" * 5, ["a.ts", "b.ts", "c.ts"], False)
    assert not rp.is_light("+x\n" * 5, ["auth.ts"], True)


def test_quick_pass_note_is_attached_and_asks_for_few_reads():
    assert "QUICK PASS" in rp.LIGHT_NOTE and "at most 3 reads" in rp.LIGHT_NOTE


async def test_review_sub_agents_are_not_routed_by_the_words_in_their_brief(monkeypatch):
    seen = []

    class Boom(Exception):
        pass

    class FakeAgent:
        def __init__(self, cfg):
            seen.append(cfg)
            raise Boom

    import agent.agent as agent_mod
    from config.config import Config
    monkeypatch.setattr(agent_mod, "Agent", FakeAgent)
    assert await rp.ask(Config(), "check the authorization and security of this", "default", 3, 1000) == ""
    assert seen[0].auto_route is False
