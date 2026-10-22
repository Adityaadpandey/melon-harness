from agent import review_pipeline as rp


def test_plausible_survives_the_verifier_and_a_low_confidence_one_is_not_dropped():
    got = rp.parse_verdicts("1 | PLAUSIBLE | a.sh:3 prune can fail under set -e | deploys | ONGOING | NEW | NONE | CONF 30", 1)
    assert got[1][0] == "PLAUSIBLE"
    f = rp.Finding("MEDIUM", "a.sh:3", "prune fails the step", "guard it")
    rp.apply_verdicts([f], "1 | PLAUSIBLE | a.sh:3 x | deploys | ONGOING | NEW | NONE | CONF 30", 60, {id(f): 1})
    assert f.verdict == "PLAUSIBLE"
    assert "unverified" in rp.compose([f], 1)
