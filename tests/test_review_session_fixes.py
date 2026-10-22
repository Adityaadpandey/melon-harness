from agent.grounding import wants_change


def test_reviewing_the_change_is_not_asking_for_one():
    assert not wants_change("review the change i have") and not wants_change("audit the changes")
    assert wants_change("review and fix the bug")
