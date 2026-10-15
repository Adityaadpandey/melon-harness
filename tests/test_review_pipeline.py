import subprocess

import agent.review_pipeline as rp
from agent.agent import Agent
from agent.events import AgentEventType
from config.config import ApprovalPolicy, Config
from tests.fakes import ScriptedClient, call, text


def test_changed_files_skips_deleted_binary_and_follows_renames():
    out = " M apps/web/a.tsx\n D old.ts\n?? logo.png\nR  was.ts -> now.ts\n?? pnpm-lock.yaml\n"
    assert rp.changed_files(out)[:2] == ["apps/web/a.tsx", "now.ts"] and "logo.png" not in rp.changed_files(out) and "old.ts" not in rp.changed_files(out)


def test_files_group_by_area_and_never_exceed_the_reviewer_limit():
    files = ["apps/server/a.ts", "apps/server/b.ts", "apps/web/x/y.tsx", "apps/crm/z.tsx", "docs/readme.md", "README.md"]
    groups = rp.group_files(files)
    assert len(groups) <= rp.MAX_GROUPS
    assert sorted(f for _, fs in groups for f in fs) == sorted(files)  # nothing lost when areas are folded together


def test_findings_and_verdicts_parse_from_loose_model_output():
    found = rp.parse_findings("1. HIGH | a.ts:4 | insert can throw and 500 the list | catch it\nnoise line\n- low | b.ts:2 | stale type | update")
    assert [(f.severity, f.where) for f in found] == [("HIGH", "a.ts:4"), ("LOW", "b.ts:2")]
    assert rp.parse_findings("NONE") == []
    got = rp.parse_verdicts("1 | REJECTED | a.ts:9 already wrapped in try/catch | n/a\n2 | CONFIRMED | b.ts:2 no catch | dashboard users", 3)
    assert got[1][0] == "REJECTED" and "try/catch" in got[1][1] and got[2][0] == "CONFIRMED" and got[2][2] == "dashboard users"
    assert got[3][0] == "UNCLEAR"  # a claim the verifier skipped stays unproven
    assert rp.parse_verdicts("no format at all", 1)[1][0] == "UNCLEAR"


def test_compose_keeps_confirmed_downgrades_unproven_and_never_lists_rejected_as_findings():
    f = [
        rp.Finding("HIGH", "a.ts:4", "insert 500s the list", "catch", "CONFIRMED", "a.ts:4 awaited, no catch", "dashboard users"),
        rp.Finding("HIGH", "b.ts:9", "guessed race", "lock", "UNCLEAR"),
        rp.Finding("MEDIUM", "c.ts:1", "redundant chain", "collapse", "REJECTED", "c.ts:1 covers an older server"),
    ]
    out = rp.compose(f, 2)
    assert "[confirmed] HIGH a.ts:4" in out and "affects: dashboard users" in out
    assert "[unverified] MEDIUM b.ts:9" in out  # a HIGH nobody proved is at most MEDIUM
    assert "[rejected] c.ts:1" in out and "[confirmed] MEDIUM c.ts:1" not in out


async def test_pipeline_runs_reviewers_then_verifiers_and_hands_the_main_agent_only_what_survived(tmp_path, monkeypatch):
    subprocess.run("git init -q && git config user.email a@b.c && git config user.name t", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("x\n")
    subprocess.run("git add . && git commit -qm init", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("y\n")
    prompts = []

    async def fake_ask(config, prompt, role, max_turns, max_tokens, disabled_skills=None):
        prompts.append((role, prompt))
        if "claims these defects" in prompt:
            return "1 | CONFIRMED | a.ts:1 no catch | dashboard\n2 | REJECTED | a.ts:1 handled elsewhere | n/a"
        return "HIGH | a.ts:1 | write can throw and 500 the page | catch\nHIGH | a.ts:1 | a guess about a race | lock"

    monkeypatch.setattr(rp, "ask", fake_ask)
    cfg = Config(pi_mode=True, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([])  # the answer is built from the verified findings: the model is not called again
        events = [e async for e in agent.run("review the changes")]
        notices = [e.data.get("message", "") for e in events if e.type == AgentEventType.NOTICE]
        assert any("Reviewing 1 changed file" in n for n in notices) and any("1 confirmed, 1 rejected" in n for n in notices)
        assert agent.session.client.requests == []  # a read-only review is not followed by a model call or a tool loop
        done = [e.data.get("content") for e in events if e.type == AgentEventType.TEXT_COMPLETE]
        assert done == ["HIGH a.ts:1  write can throw and 500 the page -> catch\n\n1 more claim(s) were ruled out by the verifier."]
        sent = " ".join(str(m.content) for m in agent.session.context_manager._messages if m.role == "user")
        assert "[confirmed] HIGH a.ts:1" in sent and "[rejected] a.ts:1" in sent
    assert [r for r, p in prompts if "claims these defects" in p] == ["default"]  # ONE verifier, on the fast model
    assert sum("You are reviewing" in p for _, p in prompts) == 1  # and one reviewer for a small diff


async def test_a_named_scope_or_a_clean_tree_skips_the_pipeline(tmp_path, monkeypatch):
    called = []

    async def fake_ask(*a, **k):
        called.append(1)
        return "NONE"

    monkeypatch.setattr(rp, "ask", fake_ask)
    cfg = Config(pi_mode=True, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([text("No issues found"), text("No issues found")])
        _ = [e async for e in agent.run("review my last 3 commits")]
        _ = [e async for e in agent.run("review the changes")]  # not a git repo / nothing changed
    assert not called


async def test_pipeline_sub_agents_do_not_spawn_their_own_challengers_or_scouts(tmp_path, monkeypatch):
    import agent.agent as agent_mod

    seen = {}

    class Stub:
        def __init__(self, cfg):
            seen["cfg"] = cfg

        async def __aenter__(self):
            return self

        async def __aexit__(self, *a):
            return False

        async def run(self, prompt):
            return
            yield

    monkeypatch.setattr(agent_mod, "Agent", Stub)
    cfg = Config(pi_mode=True, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_challenge=True, auto_scout=True)
    await rp.ask(cfg, "p", "review", 4, 5000)
    assert seen["cfg"].auto_challenge is False and seen["cfg"].auto_scout is False and seen["cfg"].review_pipeline is False


def test_format_answer_is_one_line_per_finding_caps_unproven_ones_and_says_when_nothing_is_left():
    f = [
        rp.Finding("HIGH", "a.ts:4", "insert 500s the list", "catch", "CONFIRMED"),
        rp.Finding("HIGH", "b.ts:9", "guessed race", "lock", "UNCLEAR"),
        rp.Finding("MEDIUM", "c.ts:1", "redundant chain", "collapse", "REJECTED"),
    ]
    assert rp.format_answer(f).split("\n\n") == ["HIGH a.ts:4  insert 500s the list -> catch", "MEDIUM (unverified) b.ts:9  guessed race -> lock", "1 more claim(s) were ruled out by the verifier."]
    assert rp.format_answer([]) == "No issues found."
    assert rp.format_answer([rp.Finding("LOW", "c.ts:1", "x", "", "REJECTED")]).startswith("No issues found. The verifier ruled out 1")


async def test_a_reviewer_that_returns_nothing_is_a_failure_and_the_normal_review_loop_runs(tmp_path, monkeypatch):
    subprocess.run("git init -q && git config user.email a@b.c && git config user.name t", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("x\n")
    subprocess.run("git add . && git commit -qm init", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("y\n")

    async def dead_ask(*a, **k):
        return ""  # a crashed or stopped reviewer

    monkeypatch.setattr(rp, "ask", dead_ask)
    cfg = Config(pi_mode=True, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([text("HIGH a.ts:1  found by the normal loop -> fix")])
        events = [e async for e in agent.run("review the changes")]
        done = [e.data.get("content") for e in events if e.type == AgentEventType.TEXT_COMPLETE]
        assert done == ["HIGH a.ts:1  found by the normal loop -> fix"]  # not a false "No issues found"


def test_a_self_healing_or_already_present_failure_is_capped_at_medium_and_says_why():
    got = rp.parse_verdicts("1 | CONFIRMED | a.ts:4 no catch | every visitor | TEMPORARY (cache entries before deploy, 15 min) | NEW\n2 | CONFIRMED | b.ts:2 no catch | all | ONGOING | ALREADY_AT_HEAD\n3 | CONFIRMED | c.ts:1 x | all | ONGOING | NEW", 3)
    assert got[1][3] == "cache entries before deploy, 15 min" and got[2][4] is True and got[3][3:] == ("", False, 0)
    f = [rp.Finding("HIGH", "a.ts:4", "backfill can 500", "bump the key", "CONFIRMED", temporary="15 min"),
         rp.Finding("HIGH", "b.ts:2", "write throws", "catch", "CONFIRMED", at_head=True),
         rp.Finding("HIGH", "c.ts:1", "proven and lasting", "fix", "CONFIRMED")]
    lines = rp.format_answer(f).split("\n\n")
    assert lines[0].startswith("HIGH c.ts:1") and lines[1].startswith("MEDIUM (temporary: 15 min) a.ts:4") and "already did this" in lines[2]


def test_the_same_cause_in_one_file_is_one_finding_naming_every_line():
    a = rp.Finding("HIGH", "svc/read.ts:121-126", "backfill 500s getEventById", "catch the short-link write")
    b = rp.Finding("HIGH", "svc/read.ts:189-198", "same in getPublicEventDetail", "wrap the short-link write in catch")
    c = rp.Finding("MEDIUM", "svc/other.ts:3", "unrelated", "catch the short-link write")
    got = rp.merge_duplicates([a, b, c])
    assert [f.where for f in got] == ["svc/read.ts:121-126, 189-198", "svc/other.ts:3"]


def test_the_verifier_can_fold_claims_with_one_cause_into_one_finding_even_across_files():
    got = rp.parse_verdicts("1 | CONFIRMED | a.ts:4 no catch | all | ONGOING | NEW | NONE\n2 | CONFIRMED | b.ts:2 same | all | TEMPORARY (15 min) | ALREADY_AT_HEAD | SAME_AS 1", 2)
    assert got[2][5] == 1 and got[1][5] == 0
    a = rp.Finding("HIGH", "a.ts:4", "no catch", "wrap", "CONFIRMED")
    b = rp.Finding("MEDIUM", "b.ts:2", "same", "wrap", "CONFIRMED", temporary="15 min", at_head=True, same_as=1)
    c = rp.Finding("LOW", "c.ts:1", "other", "x", "CONFIRMED")
    one = rp.merge_same_cause([a, b, c])
    assert [f.where for f in one] == ["a.ts:4, b.ts:2", "c.ts:1"] and one[0].temporary == "" and not one[0].at_head


async def test_a_near_miss_path_is_mended_and_an_unknown_one_is_marked(tmp_path):
    import subprocess

    (tmp_path / "svc" / "event").mkdir(parents=True)
    (tmp_path / "svc" / "event" / "disc.ts").write_text("x")
    subprocess.run(["git", "init", "-q"], cwd=tmp_path)
    subprocess.run(["git", "add", "-A"], cwd=tmp_path)
    f = [rp.Finding("HIGH", "svc/disc.ts:9", "w", "f"), rp.Finding("HIGH", "svc/nope.ts:1", "w", "f"), rp.Finding("HIGH", "svc/event/disc.ts:2", "w", "f")]
    await rp.fix_paths(f, tmp_path)
    assert [x.where for x in f] == ["svc/event/disc.ts:9", "svc/nope.ts:1 (file not found)", "svc/event/disc.ts:2"]
