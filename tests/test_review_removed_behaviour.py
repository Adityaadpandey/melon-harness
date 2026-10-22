from agent import review_pipeline as rp


def test_reviewer_and_triage_look_for_removed_behaviour():
    assert "Removed behaviour" in rp.LENSES[0][1] and "deleted or moved guard" in rp.TRIAGE_PROMPT
