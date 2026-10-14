from agent.challenge import Objection, is_risky, keep_grounded, parse_objections, render

REPLY = """OBJECTIONS:
1. [verified] drops the unique index | evidence: db/schema.sql:2
2. [verified] claims a rule in ghost/file.py | evidence: ghost/file.py:9
3. [verified] says it without any citation | evidence: none
4. [plausible] callers may break | evidence: none
5. [guess] an old client may send it | evidence: none
"""


async def test_cited_or_dropped_and_uncited_claims_never_read_as_facts(tmp_path):
    (tmp_path / "db").mkdir()
    (tmp_path / "db" / "schema.sql").write_text("create table u();\ncreate unique index i on u(a);\n")
    kept, dropped = await keep_grounded(parse_objections(REPLY), tmp_path)
    assert [(o.confidence, o.text[:10]) for o in kept] == [("verified", "drops the "), ("guess", "callers ma"), ("guess", "an old cli")]
    assert dropped == 2  # the invented file and the uncited "verified"
    assert "2 more removed" in render(kept, dropped)


def test_risky_requests_are_the_design_choices_not_small_fixes():
    assert is_risky("should we replace the queue with cron instead of workers?")
    assert is_risky("I think we should migrate to postgres")
    assert not is_risky("fix the typo in the header")
    assert parse_objections("OBJECTIONS: NONE") == []
