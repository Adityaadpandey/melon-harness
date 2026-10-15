import subprocess

import pytest

from agent.agent import Agent
from agent.events import AgentEventType
from agent.grounding import extract_refs, verify_references
from config.config import ApprovalPolicy, Config
from tests.fakes import ScriptedClient, call, text


def git(p, *a):
    subprocess.run(["git", *a], cwd=p, check=True, capture_output=True)


@pytest.fixture
def repo(tmp_path):
    git(tmp_path, "init", "-q")
    (tmp_path / "apps" / "server" / "src").mkdir(parents=True)
    (tmp_path / "apps" / "server" / "src" / "app.ts").write_text("\n".join(f"line {i}" for i in range(1, 51)))
    (tmp_path / "README.md").write_text("hi\n")
    return tmp_path


async def test_references_resolve_when_started_in_a_folder_that_holds_the_repo(repo):
    parent = repo.parent  # the repo sits one level below the working directory, which is not a repo
    n, problems = await verify_references("See apps/server/src/app.ts:12 and src/ghost.ts:3.", parent)
    assert n == 2 and [p.ref for p in problems] == ["src/ghost.ts:3"]


def test_extracts_paths_and_lines_but_not_urls_versions_or_bare_names():
    refs = extract_refs("See `src/app.ts:12-30`, README.md:1, https://example.com/a/b.ts:9, v1.2.3, and index.ts alone, plus lib/x.py")
    assert ("src/app.ts", 12, 30) in refs and ("README.md", 1, None) in refs and ("lib/x.py", None, None) in refs
    assert not any("example" in r[0] or r[0] == "index.ts" for r in refs)


async def test_verifies_existing_missing_and_out_of_range_references(repo):
    n, problems = await verify_references(
        "Fixed in apps/server/src/app.ts:12 and src/app.ts:49, but see src/ghost.ts:3 and apps/server/src/app.ts:999 and README.md.", repo
    )
    bad = {p.ref: p.why for p in problems}
    assert n == 4  # a bare `README.md` is too ambiguous to judge, so it is skipped
    assert set(bad) == {"src/ghost.ts:3", "apps/server/src/app.ts:999"}  # sub-app relative citations resolve; fabrications do not
    assert "50 lines" in bad["apps/server/src/app.ts:999"]


async def test_fabricated_reference_is_sent_back_once_then_reported(repo):
    cfg = Config(pi_mode=False, cwd=repo, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([text("The bug is in src/invented.ts:7."), text("It is in apps/server/src/app.ts:12.")])
        evs = [e async for e in a.run("where is the bug?")]
        rep = next(e for e in evs if e.type == AgentEventType.AGENT_END).data["report"]
        assert rep["grounding"]["unverified"] == [] and rep["grounding"]["checked"] == 1
        assert "do not exist" in a.session.client.requests[1][-1]["content"]
        assert any("could not be verified" in e.data.get("message", "") for e in evs if e.type == AgentEventType.NOTICE)


async def test_uncorrected_fabrication_stays_in_the_report_not_hidden(repo):
    cfg = Config(pi_mode=False, cwd=repo, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([text("See src/invented.ts:7."), text("Still src/invented.ts:7.")])
        evs = [e async for e in a.run("q")]
        rep = next(e for e in evs if e.type == AgentEventType.AGENT_END).data["report"]
        assert len(rep["grounding"]["unverified"]) == 1 and "invented.ts" in rep["grounding"]["unverified"][0]


async def test_edit_requires_a_prior_read_and_detects_changes_since(repo):
    f = repo / "a.py"
    f.write_text("x = 1\n")
    cfg = Config(pi_mode=False, cwd=repo, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as a:
        reg, hook = a.session.tool_registry, a.session.hook_system
        edit = lambda: reg.invoke("edit", {"path": "a.py", "old_string": "x = 1", "new_string": "x = 2"}, repo, hook, a.session.approval_manager)
        r = await edit()
        assert not r.success and "Read a.py with read_file" in r.error and f.read_text() == "x = 1\n"
        await reg.invoke("read_file", {"path": "a.py"}, repo, hook)
        f.write_text("x = 1  # edited by the user meanwhile\n")
        r = await edit()
        assert not r.success and "changed on disk" in r.error
        await reg.invoke("read_file", {"path": "a.py"}, repo, hook)
        assert (await edit()).success and f.read_text() == "x = 2  # edited by the user meanwhile\n"
        r2 = await reg.invoke("edit", {"path": "a.py", "old_string": "x = 2", "new_string": "x = 3"}, repo, hook, a.session.approval_manager)
        assert r2.success  # our own write counts as 'known content'
        w = await reg.invoke("write_file", {"path": "brand_new.py", "content": "1"}, repo, hook, a.session.approval_manager)
        assert w.success  # creating a file needs no read


def test_nudge_only_for_change_requests_with_source_code_in_the_reply():
    from agent.grounding import has_file_code, wants_change

    assert wants_change("fix the pagination bug") and wants_change("add a remove command to the CLI")
    assert not wants_change("Read-only. Open .env and tell me DATABASE_URL")
    assert not wants_change("explain how check-in works") and not wants_change("where is the discount validated?")
    assert has_file_code("Here:\n```python\nx = 1\n```\n") and has_file_code("```ts\nconst a = 1\n```")
    assert not has_file_code("Run this:\n```bash\npsql -c 'select 1'\n```\n") and not has_file_code("```sql\nSELECT 1;\n```")
    assert not has_file_code("no code here")


def test_a_review_that_dismisses_its_own_findings_is_flagged_but_a_clean_one_is_not():
    from agent.grounding import shows_second_thoughts

    messy = "high a.ts:1 keys clash ... actually a non-issue. Skip.\nmedium b.ts:2 extra insert. Acceptable; not a bug."
    assert shows_second_thoughts("review my changes", messy)
    assert not shows_second_thoughts("explain the diff", messy)  # only reviews are judged
    assert not shows_second_thoughts("review my changes", "high a.ts:1 createMany failure 500s the page -> catch and fall back")


def test_single_dismissals_and_a_final_answer_header_are_caught_but_no_issues_found_is_not():
    from agent.grounding import shows_second_thoughts

    assert shows_second_thoughts("review it", "HIGH a.ts:1 reads a cached ref. Consistent. No defect.")
    assert shows_second_thoughts("review it", "MEDIUM a.ts:9 two entries, coincidence works, not a real defect")
    assert shows_second_thoughts("review it", "Final answer (verified only):\nHIGH a.ts:1 real thing -> fix")
    assert not shows_second_thoughts("review it", "No issues found. Not run: tsc.")
