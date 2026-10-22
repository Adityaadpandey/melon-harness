import subprocess
from pathlib import Path

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

    async def fake_ask(config, prompt, role, max_turns, max_tokens, disabled_skills=None, **k):
        prompts.append((role, prompt))
        if "claims these defects" in prompt:
            return "1 | CONFIRMED | a.ts:1 no catch | dashboard\n2 | REJECTED | a.ts:1 handled elsewhere | n/a"
        return "HIGH | a.ts:1 | write can throw and 500 the page | catch\nHIGH | a.ts:1 | a guess about a race | lock"

    monkeypatch.setattr(rp, "ask", fake_ask)
    cfg = Config(pi_mode=True, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, review_lenses=4)
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([])  # the answer is built from the verified findings: the model is not called again
        events = [e async for e in agent.run("review the changes")]
        notices = [e.data.get("message", "") for e in events if e.type == AgentEventType.NOTICE]
        assert any("Reviewing 1 changed file" in n for n in notices) and any("1 confirmed, 1 rejected" in n for n in notices)
        assert agent.session.client.requests == []  # a read-only review is not followed by a model call or a tool loop
        done = [e.data.get("content") for e in events if e.type == AgentEventType.TEXT_COMPLETE]
        assert len(done) == 1 and done[0].startswith("## Code review: 1 issue") and "**1. `a.ts:1`  write can throw and 500 the page**" in done[0] and "- **Fix:** catch" in done[0]
        assert "I read 1 file(s)" in done[0] and "did not run typecheck" in done[0]
        sent = " ".join(str(m.content) for m in agent.session.context_manager._messages if m.role == "user")
        assert "[confirmed] HIGH a.ts:1" in sent and "[rejected] a.ts:1" in sent
    assert [r for r, p in prompts if "claims these defects" in p] == ["default"]  # ONE verifier, on the fast model
    reviewers = [p for _, p in prompts if "principal engineer reviewing" in p]
    assert len(reviewers) == 4  # one reviewer per angle, even for a small diff
    assert {w for w in ("LENS: behaviour", "LENS: approach", "LENS: failures", "LENS: rules") if any(w in p for p in reviewers)} == {"LENS: behaviour", "LENS: approach", "LENS: failures", "LENS: rules"}


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
    got = rp.format_answer(f)
    assert got.startswith("## Code review: 2 issues\n\n### Likely bugs\n**1. `a.ts:4`  insert 500s the list**\n- **Fix:** catch")
    assert "### Not verified, worth a look\n**2. `b.ts:9`  guessed race**\n- **Fix:** lock" in got and got.endswith("Want me to fix it?")
    assert rp.format_answer([]) == "## Code review\n\nNo issues found."
    assert rp.format_answer([rp.Finding("LOW", "c.ts:1", "x", "", "REJECTED")]).startswith("## Code review\n\nNo issues found. I checked 1")


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
    assert got[1][3] == "cache entries before deploy, 15 min" and got[2][4] is True and got[3][3:] == ("", False, 0, 100)
    f = [rp.Finding("HIGH", "a.ts:4", "backfill can 500", "bump the key", "CONFIRMED", temporary="15 min"),
         rp.Finding("HIGH", "b.ts:2", "write throws", "catch", "CONFIRMED", at_head=True),
         rp.Finding("HIGH", "c.ts:1", "proven and lasting", "fix", "CONFIRMED")]
    text = rp.format_answer(f)
    assert text.index("c.ts:1`  proven") < text.index("a.ts:4`  backfill") and "goes away by itself (15 min)" in text


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


async def test_a_file_moved_into_a_new_folder_is_one_move_line_not_a_deletion_plus_a_new_file(tmp_path):
    import subprocess

    def git(*a):
        subprocess.run(["git", "-C", str(tmp_path), *a], check=True, capture_output=True)

    git("init", "-q")
    git("config", "user.email", "t@t")
    git("config", "user.name", "t")
    (tmp_path / "events").mkdir()
    (tmp_path / "events/page.tsx").write_text("export default function P() {}\n")
    (tmp_path / "events/other.tsx").write_text("old\n")
    git("add", ".")
    git("commit", "-qm", "base")
    (tmp_path / "events/(list)").mkdir()
    (tmp_path / "events/page.tsx").rename(tmp_path / "events/(list)/page.tsx")
    (tmp_path / "events/other.tsx").write_text("new\n")

    porcelain = await rp._git(tmp_path, "status", "--porcelain", "-uall")
    moves, deleted, added = await rp.pair_moves(tmp_path, rp.deleted_files(porcelain), rp.changed_files(porcelain))
    assert moves == [("events/page.tsx", "events/(list)/page.tsx")] and deleted == [] and added == ["events/other.tsx"]
    (tmp_path / "events/[id]").mkdir()
    (tmp_path / "events/[id]/page.tsx").write_text("x\n")
    git("add", "events/[id]")
    stayed = await rp.left_behind(tmp_path, moves)
    assert "events/other.tsx" in stayed and "events/[id]/page.tsx" in stayed and "events/page.tsx" not in stayed
    out = rp.frame("No issues found.", "Moves the list page into a route group.", added, moves, deleted)
    assert "events/page.tsx -> events/(list)/page.tsx" in out and "did not run typecheck" in out and "git add -A" in out and out.startswith("**What the change does")


def test_review_context_brings_the_folders_rules_and_the_matching_playbook(tmp_path):
    from agent.review import review_context

    (tmp_path / "apps" / "web").mkdir(parents=True)
    (tmp_path / "AGENTS.md").write_text("Never log auth tokens anywhere in the server code.\n")
    (tmp_path / "apps" / "web" / "AGENTS.md").write_text("Use pnpm, never npm, in the web app scripts.\n")
    (tmp_path / "apps" / "web" / "a.tsx").write_text("x\n")
    ctx = review_context(Config(cwd=tmp_path), "review the changes", ["apps/web/a.tsx"])
    assert "Never log auth tokens" in ctx and "Use pnpm" in ctx and "shown once" not in ctx
    assert ctx.index("Never log") < ctx.index("Use pnpm")  # root first, the deeper folder after


def test_duplicate_findings_from_several_angles_name_the_place_once():
    found = [rp.Finding("HIGH", "a.ts:1", "write can throw and 500 the page", "catch it") for _ in range(4)]
    merged = rp.merge_duplicates(found)
    assert len(merged) == 1 and merged[0].where == "a.ts:1"


def test_by_default_one_reviewer_carries_every_angle_beside_a_contract_tracer_and_a_pinned_count_fans_out():
    cfg = Config()
    names, text = rp.pick_lenses(cfg, "+x\n" * 300, ["a.ts", "b.ts", "c.ts"], "PROJECT RULES")[0]
    assert names == "review" and [n for n, _ in rp.pick_lenses(cfg, "+x\n" * 300, ["a.ts"], "")] == ["review", "contracts"]
    assert all(w in text for w in ("LENS: behaviour", "LENS: approach", "LENS: failures", "LENS: rules")) and "LENS: history" not in text
    assert "LENS: history" in rp.pick_lenses(cfg, "+x\n", ["a.ts"], "")[0][1]  # no rules to audit: read the history instead
    assert "LENS: security" in rp.pick_lenses(cfg, "+check the jwt token\n", ["auth.ts"], "")[0][1]
    assert [n for n, _ in rp.pick_lenses(Config(review_lenses=3), "+x\n", ["a.ts"], "")] == ["behaviour", "approach", "failures"]
    assert [n for n, _ in rp.pick_lenses(Config(review_lenses=1), "+jwt\n", ["auth.ts"], "")] == ["behaviour"]


def test_verdicts_carry_a_confidence_and_a_low_one_is_dropped_from_the_answer():
    got = rp.parse_verdicts("1 | CONFIRMED | a.ts:4 no catch | users | ONGOING | NEW | NONE | CONF 45\n2 | CONFIRMED | b.ts:1 x | all | ONGOING | NEW | NONE", 2)
    assert got[1][6] == 45 and got[2][6] == 100  # a reply without a score reads as sure


async def test_a_claim_the_verifier_scores_below_the_cutoff_is_ruled_out_and_remembered(tmp_path, monkeypatch):
    subprocess.run("git init -q && git config user.email a@b.c && git config user.name t", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("x\n")
    subprocess.run("git add . && git commit -qm init", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("y\n")
    import agent.review_memory as rm
    monkeypatch.setattr(rm, "path_for", lambda cwd, base=None: tmp_path / "mem.json")

    async def fake_ask(config, prompt, role, max_turns, max_tokens, disabled_skills=None):
        if "claims these defects" in prompt:
            return "1 | CONFIRMED | a.ts:1 maybe | few | ONGOING | NEW | NONE | CONF 30"
        return "MEDIUM | a.ts:1 | a rare race | lock"

    monkeypatch.setattr(rp, "ask", fake_ask)
    cfg = Config(pi_mode=True, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, review_lenses=1)
    out = [x async for x in rp.run_review_pipeline(cfg, "review the changes")]
    answer = next(v for k, v in out if k == "answer")
    assert answer.startswith("## Code review\n\nNo issues found.") and "checked 1 suspected" in answer
    assert rm.render(tmp_path) == ""  # a low score is "not worth reporting", not "proven wrong": it is not remembered


def test_check_findings_blame_only_failures_that_name_a_changed_file():
    from agent.verify import CheckResult

    bad = CheckResult("npx tsc --noEmit", False, 2, 4.0, "src/a.ts(12,5): error TS2322: nope\nsrc/old.ts(3,1): error TS1")
    got = rp.check_findings([bad, CheckResult("lint", True, 0, 1.0, "")], ["src/a.ts"])
    assert [(f.where, f.verdict) for f in got] == [("src/a.ts:12", "CONFIRMED")]
    assert rp.check_findings([bad], ["src/other.ts"]) == []  # old debt elsewhere is not this change's


def test_claude_code_runs_read_only_with_the_repos_secret_rules_and_its_json_is_parsed():
    import agent.review_claude as rc

    cmd = rc.build_command(Config(review_claude_model="sonnet"), "hello", "sonnet")
    assert cmd[:3] == ["claude", "-p", "hello"] and "Edit" in cmd[cmd.index("--disallowedTools"):] and "Bash(git blame:*)" in cmd
    assert any(c.startswith("Read(**/.env") for c in cmd) and cmd[-2:] == ["--model", "sonnet"]
    assert rc.parse_output('{"result": "SUMMARY: x\\nNONE", "usage": {"input_tokens": 5, "output_tokens": 7}}') == ("SUMMARY: x\nNONE", 12)
    assert rc.parse_output('{"is_error": true, "result": "boom"}') == ("", 0) and rc.parse_output("not json") == ("", 0)


def test_review_memory_keeps_a_ruled_out_claim_once_and_shows_it_to_the_next_review(tmp_path):
    import agent.review_memory as rm

    rm.remember(tmp_path, [("a.ts:4", "unguarded write", "caller wraps it")], base=tmp_path / "d")
    rm.remember(tmp_path, [("a.ts:9", "unguarded write", "caller wraps it")], base=tmp_path / "d")  # same file and wording
    assert len(rm.load(tmp_path, tmp_path / "d")) == 1
    assert "a.ts:4: unguarded write" in rm.render(tmp_path, tmp_path / "d") and rm.render(tmp_path / "x", tmp_path / "e") == ""


async def test_review_targets_resolve_last_commits_a_commit_and_the_branch(tmp_path):
    import agent.review_target as rt

    def sh(c): subprocess.run(c, shell=True, cwd=tmp_path, check=True, capture_output=True)
    sh("git init -q -b main && git config user.email a@b.c && git config user.name t")
    for i in range(3):
        (tmp_path / f"f{i}.ts").write_text(f"{i}\n")
        sh(f"git add . && git commit -qm c{i}")
    t = await rt.resolve(tmp_path, "review my last 2 commits", ())
    assert t and t.rev == ("HEAD~2", "HEAD") and sorted(t.files) == ["f1.ts", "f2.ts"] and not t.includes_worktree
    sha = subprocess.run("git rev-parse --short HEAD", shell=True, cwd=tmp_path, capture_output=True, text=True).stdout.strip()
    one = await rt.resolve(tmp_path, f"review commit {sha}", ())
    assert one and one.files == ["f2.ts"]
    assert await rt.resolve(tmp_path, "review this branch", ()) is None  # on main there is no branch to compare
    sh("git checkout -q -b feat")
    (tmp_path / "g.ts").write_text("new\n")
    sh("git add . && git commit -qm g")
    br = await rt.resolve(tmp_path, "review this branch", ())
    assert br and br.files == ["g.ts"] and br.includes_worktree
    assert await rt.resolve(tmp_path, "review the changes", ()) is None  # the working tree is the default, not a target
    assert await rt.resolve(tmp_path, "review my last 9 commits", ()) is None  # more commits than exist


def test_review_bench_scores_recall_and_noise():
    from bench.review_bench import CASES, score

    case = next(c for c in CASES if c.id == "pagination")
    got = score(case, "HIGH src/paging.py:3  page 1 skips the first page -> use (page - 1)\nLOW src/other.py:1  naming")
    assert got == {"planted": 1, "found": 1, "findings": 2, "noise": 1}
    control = next(c for c in CASES if c.id == "control")
    assert score(control, "No issues found.") == {"planted": 0, "found": 0, "findings": 0, "noise": 0}


def test_claude_code_never_inherits_a_gateway_endpoint_or_key(monkeypatch):
    import agent.review_claude as rc

    for k in ("ANTHROPIC_BASE_URL", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"):
        monkeypatch.setenv(k, "x")
    monkeypatch.setenv("API_KEY", "melon-gateway-key")
    env = rc.clean_env()
    assert not any(k.startswith("ANTHROPIC_") for k in env) and env["API_KEY"] == "melon-gateway-key"  # melon's own key is untouched, Claude Code never sees an override


async def test_a_pr_is_diffed_against_the_branch_it_merges_into_not_main(tmp_path, monkeypatch):
    import agent.review_target as rt
    run = lambda c: subprocess.run(c, shell=True, cwd=tmp_path, capture_output=True, check=True)
    run("git init -q -b main && git config user.email a@b.c && git config user.name t")
    (tmp_path / "a.txt").write_text("1\n"); run("git add . && git commit -qm base")
    run("git checkout -qb develop"); (tmp_path / "dev.txt").write_text("d\n"); run("git add . && git commit -qm dev")
    run("git checkout -qb feat"); (tmp_path / "f.txt").write_text("f\n"); run("git add . && git commit -qm feat")

    async def info(cwd, number=""):
        return "feat", "develop"

    monkeypatch.setattr(rt, "_pr_info", info)
    got = await rt.resolve(tmp_path, "review PR #7", ())
    assert got.files == ["f.txt"] and "develop" in got.label  # dev.txt belongs to develop, not to this PR
    assert (await rt.resolve(tmp_path, "review this branch", ())).files == ["f.txt"]


def test_frame_shows_what_was_done_well_and_what_was_set_aside():
    out = rp.frame("Verdict: approve. No issues found.", "adds paging", ["a.ts"], [], [], None, ["a.ts:4 the cursor is opaque"], ["the retry loop looks odd but is the intent"])
    assert "**What is good:** a.ts:4 the cursor is opaque." in out and "retry loop" not in out


def test_every_surviving_finding_is_shown_with_a_heading_and_the_rest_in_bullets():
    notes = [rp.Finding("MEDIUM", f"SKILL.md:{i}", "A width rule clashes with the design doc. The doc says 1280 and the page code uses it", "reconcile", "CONFIRMED", reach="agents") for i in range(5)]
    notes.append(rp.Finding("HIGH", "page.tsx:9", "Escape dismisses the one-time passcode <tool_call>junk", "ignore Escape", "CONFIRMED"))
    out = rp.format_answer(notes)
    assert out.count("`SKILL.md:") == 5 and "left out" not in out  # nothing is dropped for length
    assert "**1. `page.tsx:9`  Escape dismisses the one-time passcode**" in out and "junk" not in out
    assert "**2. `SKILL.md:0`  A width rule clashes with the design doc**\n- **Why:** The doc says 1280 and the page code uses it." in out
    assert out.index("### Likely bugs") < out.index("### Worth fixing")
    assert rp.tidy("word " * 100, 40).endswith("...") and len(rp.tidy("word " * 100, 40)) <= 43


def test_the_reviewer_is_offered_every_skill_it_can_open(tmp_path, monkeypatch):
    from agent import review
    from agent.skills import Skill
    skills = [Skill("tunyt-web-design", "Tunyt UI rules for pages and components", "body"), Skill("code-review", "x", "y")]
    monkeypatch.setattr("agent.skills.load_skills", lambda config: skills)
    ctx = review.review_context(Config(cwd=tmp_path), "review the ui", ["a.tsx"])
    assert "MORE SKILLS" in ctx and "tunyt-web-design: Tunyt UI rules" in ctx and "code-review:" not in ctx


async def test_a_defect_the_old_code_already_had_is_not_reported(tmp_path, monkeypatch):
    subprocess.run("git init -q && git config user.email a@b.c && git config user.name t", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("x\n")
    subprocess.run("git add . && git commit -qm init", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("y\n")
    import agent.review_memory as rm
    monkeypatch.setattr(rm, "path_for", lambda cwd, base=None: tmp_path / "mem.json")

    async def fake_ask(config, prompt, role, max_turns, max_tokens, disabled_skills=None):
        if "claims these defects" in prompt:
            return "1 | CONFIRMED | a.ts:1 maybe | few | ONGOING | ALREADY_AT_HEAD | NONE | CONF 90"
        return "MEDIUM | a.ts:1 | a rare race | lock"

    monkeypatch.setattr(rp, "ask", fake_ask)
    cfg = Config(pi_mode=True, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, review_lenses=1)
    out = [x async for x in rp.run_review_pipeline(cfg, "review the changes")]
    answer = next(v for k, v in out if k == "answer")
    assert answer.startswith("## Code review\n\nNo issues found.")


def test_a_finding_that_cites_a_file_nobody_has_is_dropped_but_a_deleted_file_is_kept():
    f = [rp.Finding("HIGH", "x/nope.ts:1 (file not found)", "w", "f"), rp.Finding("HIGH", "x/gone.ts:1 (file not found)", "w", "f"), rp.Finding("HIGH", "a.ts:2", "w", "f")]
    assert [x.where for x in rp.drop_missing_files(f, ["x/gone.ts", "a.ts"])] == ["x/gone.ts:1 (file not found)", "a.ts:2"]


async def test_verifier_reads_claims_in_batches_retries_the_unclear_ones_and_keeps_what_stays_unclear_as_plausible(monkeypatch):
    calls = []

    async def fake_ask(config, prompt, role, max_turns, max_tokens, disabled_skills=None):
        calls.append(prompt)
        if "A first verifier" in prompt:  # the second look settles the first unclear claim only
            return "1 | CONFIRMED | a.ts:1 read it | users | ONGOING | NEW | NONE | CONF 80"
        if "a.ts:1 |" in prompt:
            return "\n".join(f"{n} | UNCLEAR |" for n in range(1, 7))
        return "1 | CONFIRMED | z.ts:1 ok | users | ONGOING | NEW | NONE | CONF 90"

    monkeypatch.setattr(rp, "ask", fake_ask)
    findings = [rp.Finding("HIGH", "a.ts:1", f"first {n}", "f") if n < 6 else rp.Finding("HIGH", "z.ts:1", "last", "f") for n in range(6)]
    findings += [rp.Finding("HIGH", "z.ts:1", "seventh", "f")]
    await rp.verify(Config(), "default", findings)
    assert len(calls) == 3  # two batches (6 + 1), then one retry for the unclear ones
    assert [f.verdict for f in findings] == ["CONFIRMED"] + ["PLAUSIBLE"] * 5 + ["CONFIRMED"]
    assert findings[1].evidence == "could not be settled by reading the code"


async def test_a_pr_that_is_not_checked_out_is_reviewed_from_a_throwaway_worktree_and_cleaned_up(tmp_path, monkeypatch):
    import agent.review_target as rt
    origin, work = tmp_path / "origin.git", tmp_path / "work"
    sh = lambda c, d: subprocess.run(c, shell=True, cwd=d, capture_output=True, check=True)
    tmp_path.joinpath("work").mkdir()
    sh("git init -q --bare -b main origin.git", tmp_path)
    sh(f"git init -q -b main && git config user.email a@b.c && git config user.name t && git remote add origin {origin}", work)
    (work / "a.txt").write_text("1\n"); sh("git add . && git commit -qm base && git push -q origin main", work)
    sh("git checkout -qb feat", work); (work / "f.txt").write_text("f\n"); sh("git add . && git commit -qm 'add f' && git push -q origin feat:refs/pull/7/head", work)
    sh("git checkout -q main", work)  # standing on main: the PR is not the checked-out branch

    async def info(cwd, number=""):
        return "feat", "main"

    monkeypatch.setattr(rt, "_pr_info", info)
    got = await rt.resolve(work, "review PR #7", ())
    assert got.files == ["f.txt"] and got.cwd and (got.cwd / "f.txt").read_text() == "f\n" and got.commits[0].endswith("add f")
    assert not (work / "f.txt").exists()  # the user's checkout is untouched
    await got.cleanup()
    assert not got.cwd.exists()


async def test_a_missing_pr_base_is_reported_instead_of_silently_falling_back_to_main(tmp_path, monkeypatch):
    import agent.review_target as rt
    run = lambda c: subprocess.run(c, shell=True, cwd=tmp_path, capture_output=True, check=True)
    run("git init -q -b main && git config user.email a@b.c && git config user.name t")
    (tmp_path / "a.txt").write_text("1\n"); run("git add . && git commit -qm base")
    run("git checkout -qb feat"); (tmp_path / "f.txt").write_text("f\n"); run("git add . && git commit -qm feat")

    async def info(cwd, number=""):
        return "", ""

    monkeypatch.setattr(rt, "_pr_info", info)
    got = await rt.resolve(tmp_path, "review this pr", ())
    assert got.files == ["f.txt"] and any("assumed" in n for n in got.notes)


def test_a_diff_that_lives_in_one_area_is_split_into_parts_when_it_is_clipped():
    parts = rp.split_clipped([f"apps/web/src/a{n}.ts" for n in range(7)])
    assert [n for n, _ in parts] == ["part 1", "part 2", "part 3"] and sum(len(f) for _, f in parts) == 7


def test_a_repro_reply_is_read_as_reproduced_not_reproduced_or_cannot_run():
    from agent.review_repro import parse_repro
    assert parse_repro("noise\nREPRODUCED | pnpm vitest a | expected 3x got 3 -")[0] == "REPRODUCED"
    assert parse_repro("- NOT_REPRODUCED | node x.js | prints 3x Name")[0] == "NOT_REPRODUCED"
    assert parse_repro("it probably fails")[0] == "CANNOT_RUN"


async def test_a_confirmed_high_is_run_in_a_throwaway_copy_and_one_that_does_not_reproduce_drops_to_medium(tmp_path, monkeypatch):
    run = lambda c: subprocess.run(c, shell=True, cwd=tmp_path, capture_output=True, check=True)
    run("git init -q -b main && git config user.email a@b.c && git config user.name t")
    (tmp_path / "a.ts").write_text("x\n"); (tmp_path / "node_modules").mkdir(); run("git add a.ts && git commit -qm init")
    (tmp_path / "a.ts").write_text("y\n")
    seen = {}

    async def fake_ask(config, prompt, role, max_turns, max_tokens, disabled_skills=None, tools=None, approval="never"):
        if tools:
            seen.update(cwd=Path(config.cwd), tools=tools, approval=approval, has_change=(Path(config.cwd) / "a.ts").read_text(), linked=(Path(config.cwd) / "node_modules").is_symlink())
            return "NOT_REPRODUCED | node t.js | prints the right thing" if "second" in prompt else "REPRODUCED | node t.js | got 3 want 3x"
        return ""

    monkeypatch.setattr(rp, "ask", fake_ask)
    a = rp.Finding("HIGH", "a.ts:1", "first", "f", "CONFIRMED", "read it")
    b = rp.Finding("HIGH", "a.ts:1", "second", "f", "CONFIRMED", "read it")
    c = rp.Finding("HIGH", "a.ts:1", "third", "f", "UNCLEAR")
    await rp.reproduce(Config(cwd=tmp_path), "default", tmp_path, [a, b, c], ["a.ts"], None)
    assert a.repro.startswith("reproduced:") and a.severity == "HIGH"
    assert b.repro.startswith("tried to reproduce and could not") and b.severity == "MEDIUM" and not c.repro
    assert seen["cwd"] != tmp_path and seen["has_change"] == "y\n" and seen["linked"] and seen["approval"] == "auto" and "shell" in seen["tools"]
    assert not seen["cwd"].exists() and (tmp_path / "a.ts").read_text() == "y\n"  # the copy is gone, the checkout untouched


def test_a_fix_reply_is_told_apart_from_other_messages_and_picks_the_numbered_findings():
    from agent import review_fix as rf
    for yes in ("fix them", "yes, fix it", "ok go ahead and fix these", "Fix all", "fix the issues", "fix 1 and 3", "please fix #2"):
        assert rf.asks_for_fix(yes), yes
    for no in ("don't fix anything", "how would you fix this?", "review and fix the PR", "what does fix mean here"):
        assert not rf.asks_for_fix(no), no
    shown = [rp.Finding("HIGH", "a.ts:1", "w1", "f1", "CONFIRMED"), rp.Finding("MEDIUM", "b.ts:2", "w2", "f2", "CONFIRMED"), rp.Finding("MEDIUM", "c.ts:3", "w3", "f3", "UNCLEAR")]
    assert [f.where for f in rf.chosen("fix them", shown)] == ["a.ts:1", "b.ts:2"]  # a guess is only fixed when named
    assert [f.where for f in rf.chosen("fix 3 and 1", shown)] == ["c.ts:3", "a.ts:1"]
    assert rf.review_and_fix("review this pr and fix what you find") and not rf.review_and_fix("review this pr, don't fix anything")
    text = rf.brief(shown[:1])
    assert "a.ts:1" in text and "ROOT CAUSE" in text and "`advisor`" in text and "DONE MEANS" in text


def test_the_numbers_in_the_answer_are_the_numbers_fix_refers_to():
    f = [rp.Finding("MEDIUM", "m.ts:1", "medium one. why", "f", "CONFIRMED"), rp.Finding("HIGH", "h.ts:1", "high one. why", "f", "CONFIRMED"), rp.Finding("HIGH", "g.ts:1", "guess. why", "f", "UNCLEAR"), rp.Finding("HIGH", "r.ts:1", "no", "f", "REJECTED")]
    order = rp.display_order(f)
    assert [x.where for x in order] == ["h.ts:1", "m.ts:1", "g.ts:1"]
    answer = rp.format_answer(f)
    assert answer.index("`h.ts:1`") < answer.index("`m.ts:1`") < answer.index("`g.ts:1`") and "**1. `h.ts:1`" in answer


async def test_fix_after_a_review_is_gated_on_the_advisor_before_the_edit_and_again_before_the_end(tmp_path, monkeypatch):
    import agent.agent as agent_mod
    from agent.review import ReviewResult
    from agent.events import AgentEventType

    subprocess.run("git init -q && git config user.email a@b.c && git config user.name t", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("x\n")
    subprocess.run("git add . && git commit -qm init", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("y\n")

    async def fake_ask(config, prompt, role, max_turns, max_tokens, disabled_skills=None, **k):
        if "claims these defects" in prompt:
            return "1 | CONFIRMED | a.ts:1 read it | users | ONGOING | NEW | NONE | CONF 90"
        return "HIGH | a.ts:1 | the write can throw | catch it"

    seen_fixing = []

    async def fake_review(config, request, files, checks, claims="", prior="", role="review", fixing=""):
        seen_fixing.append(fixing)
        return ReviewResult("PASS", "")

    monkeypatch.setattr(rp, "ask", fake_ask)
    monkeypatch.setattr(agent_mod, "review_changes", fake_review)
    cfg = Config(pi_mode=True, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, review_repro=False, review_lenses=1)
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([])
        _ = [e async for e in agent.run("review the changes")]
        assert [f.where for f in agent._shown] == ["a.ts:1"]
        client = agent.session.client = ScriptedClient([
            call("read_file", {"path": "a.ts"}, "c1"),
            call("edit", {"path": "a.ts", "old_string": "y", "new_string": "z"}, "c2"),  # refused: no advisor yet
            call("advisor", {}, "c3"),
            text("1. the root cause is the shared writer"),  # the advisor's own reply
            call("edit", {"path": "a.ts", "old_string": "y", "new_string": "z"}, "c4"),
            text("fixed a.ts:1"),
            call("advisor", {}, "c5"),  # the end-of-fix nudge
            text("1. covered; double-check the empty case"),
            text("Done: a.ts:1 now catches the write."),
        ])
        agent.session.tool_registry.get("advisor").client = client
        events = [e async for e in agent.run("fix them")]
        notices = [e.data.get("message", "") for e in events if e.type == AgentEventType.NOTICE]
        assert any("Fixing 1 finding(s)" in n for n in notices) and any("final consultation" in n for n in notices)
        assert (tmp_path / "a.ts").read_text() == "z\n"
        results = [str(m.content) for m in agent.session.context_manager._messages if m.role == "tool"]
        assert any("Consult the advisor before you edit" in r for r in results)  # the first edit was refused
    assert seen_fixing and "a.ts:1" in seen_fixing[0]  # the reviewer of the fix is told what the fix was for


def test_the_fix_verifier_reply_is_parsed_and_a_pass_needs_every_finding_fixed_and_no_new_serious_problem():
    from agent.review_fix_verify import parse
    ok = parse("1 | FIXED | vitest a: 4 passed, fails on HEAD\nVERDICT: PASS")
    assert ok.passed and ok.ran and ok.summary() == "1/1 finding(s) shown fixed"
    half = parse("1 | FIXED | ok\n2 | NOT_FIXED | still throws on []\nNEW | MEDIUM | c.ts:12 | drops the empty state | restore it\nVERDICT: PASS")
    assert not half.passed and "2. NOT_FIXED: still throws on []" in half.feedback() and "NEW MEDIUM c.ts:12 drops the empty state -> restore it" in half.feedback()
    assert parse("1 | FIXED | ok\nNEW | LOW | c.ts:1 | naming | rename\nVERDICT: PASS").passed  # a LOW note alone does not fail a fix
    assert not parse("I looked and it seems fine").ran  # no verdict line: the verifier did not finish


async def test_the_fix_verifier_runs_in_a_copy_of_the_changed_tree_with_a_shell(tmp_path, monkeypatch):
    from agent.review_fix_verify import verify_fix
    subprocess.run("git init -q -b main && git config user.email a@b.c && git config user.name t", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("x\n")
    subprocess.run("git add . && git commit -qm init", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("fixed\n")
    seen = {}

    async def fake_ask(config, prompt, role, max_turns, max_tokens, disabled_skills=None, tools=None, approval="never"):
        seen.update(cwd=Path(config.cwd), content=(Path(config.cwd) / "a.ts").read_text(), tools=tools, approval=approval, prompt=prompt)
        return "1 | FIXED | node t.js printed ok\nVERDICT: PASS"

    monkeypatch.setattr(rp, "ask", fake_ask)
    v = await verify_fix(Config(cwd=tmp_path), "default", tmp_path, "1. HIGH a.ts:1: it throws", "I wrapped the call", ["a.ts"], "tsc=ok")
    assert v.passed and seen["cwd"] != tmp_path and seen["content"] == "fixed\n" and "shell" in seen["tools"] and seen["approval"] == "auto"
    assert "1. HIGH a.ts:1: it throws" in seen["prompt"] and "I wrapped the call" in seen["prompt"] and "tsc=ok" in seen["prompt"]
    assert not seen["cwd"].exists() and (tmp_path / "a.ts").read_text() == "fixed\n"


async def test_a_failing_fix_verdict_goes_back_to_the_author_and_the_second_round_ends_the_run(tmp_path, monkeypatch):
    import agent.agent as agent_mod
    from agent.review import ReviewResult
    from agent.review_fix_verify import FixVerdict

    subprocess.run("git init -q && git config user.email a@b.c && git config user.name t", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("x\n")
    subprocess.run("git add . && git commit -qm init", shell=True, cwd=tmp_path)
    (tmp_path / "a.ts").write_text("y\n")

    async def fake_ask(config, prompt, role, max_turns, max_tokens, disabled_skills=None, **k):
        return "1 | CONFIRMED | a.ts:1 read it | users | ONGOING | NEW | NONE | CONF 90" if "claims these defects" in prompt else "HIGH | a.ts:1 | the write can throw | catch it"

    rounds = []

    async def fake_verify(config, role, cwd, findings, claimed, files, checks):
        rounds.append(findings)
        return FixVerdict(False, [(1, "NOT_FIXED", "still throws on an empty list")]) if len(rounds) == 1 else FixVerdict(True, [(1, "FIXED", "ran it")])

    async def fake_review(*a, **k):
        return ReviewResult("PASS", "")

    monkeypatch.setattr(rp, "ask", fake_ask)
    monkeypatch.setattr(agent_mod, "review_changes", fake_review)
    monkeypatch.setattr(agent_mod, "verify_fix", fake_verify)
    cfg = Config(pi_mode=True, cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, review_repro=False, review_lenses=1)
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([])
        _ = [e async for e in agent.run("review the changes")]
        client = agent.session.client = ScriptedClient([
            call("read_file", {"path": "a.ts"}, "c1"), call("advisor", {}, "c2"), text("1. fine"),
            call("edit", {"path": "a.ts", "old_string": "y", "new_string": "z"}, "c3"), text("fixed"),
            call("advisor", {}, "c4"), text("1. ok"), text("done"),
            text("also handled the empty list"),
        ])
        agent.session.tool_registry.get("advisor").client = client
        events = [e async for e in agent.run("fix them")]
        notices = [e.data.get("message", "") for e in events if e.type == AgentEventType.NOTICE]
        assert any("Fix verifier: FAIL" in n for n in notices) and any("Fix verifier: PASS" in n for n in notices)
        sent = [str(m.content) for m in agent.session.context_manager._messages if m.role == "user"]
        assert any("A fresh verifier ran the fix and found" in s and "still throws on an empty list" in s for s in sent)
    assert len(rounds) == 2 and "a.ts:1" in rounds[0]
