from agent.verify import changed_since


def test_a_commit_is_not_a_change():
    assert changed_since({"a.ts": "h1"}, {}) == []  # committed: dirty before, clean after
    assert changed_since({}, {"a.ts": "h1"}) == ["a.ts"]
