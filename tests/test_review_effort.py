from agent import review_pipeline as rp


def test_small_change_is_light_and_a_big_or_risky_one_is_not():
    assert rp.is_light("+x\n" * 16, ["deploy.yml"], False)
    assert not rp.is_light("+x\n" * 200, ["a.ts"], False)
    assert not rp.is_light("+x\n" * 5, ["a.ts", "b.ts", "c.ts"], False)
    assert not rp.is_light("+x\n" * 5, ["auth.ts"], True)


def test_quick_pass_note_is_attached_and_asks_for_few_reads():
    assert "QUICK PASS" in rp.LIGHT_NOTE and "at most 3 reads" in rp.LIGHT_NOTE
