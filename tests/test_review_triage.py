from agent import review_pipeline as rp


def test_triage_questions_become_reviewers():
    reply = "CHECK | a.ts:10 | does the new null return break callers?\nnoise\nCHECK | b.ts:3 | is the lock still released on error?"
    checks = rp.parse_triage(reply)
    assert checks[0].startswith("a.ts:10: ") and len(checks) == 2
    assert rp.parse_triage("NONE") == []
    assert len(rp.triage_lenses(checks)) == 1  # two questions: one reviewer
    assert len(rp.triage_lenses(checks * 3)) == 2  # six: two
