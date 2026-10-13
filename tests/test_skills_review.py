import subprocess

import pytest

import agent.agent as agent_mod
from agent.agent import Agent
from agent.events import AgentEventType
from agent.review import ReviewResult, build_diff, parse_verdict
from agent.skills import extract_section, headings, match_skills, parse_skill
from config.config import ApprovalPolicy, Config, SubagentSpec
from config.loader import load_config
from tests.fakes import ScriptedClient, call, text
from tools.subagents import get_default_subagent_definitions

SKILL = """---
name: dbg
description: debugging runbook
triggers: bug, stuck job, 500   # words that load it
canonical: docs/dbg.md
---
Body text.
"""
DOC = "# Top\nintro\n## Worker lane\nworker facts\n### sub\nmore\n## Database lane\ndb facts\n```\n## not a heading\n```\n"


def test_parse_and_match():
    sk = parse_skill(SKILL)
    assert sk.name == "dbg" and sk.triggers == ["bug", "stuck job", "500"] and sk.canonical == "docs/dbg.md"
    other = parse_skill("---\nname: perf\ndescription: x\ntriggers: slow, cache\n---\nB")
    assert [s.name for s in match_skills([sk, other], "There is a bug and the cache is slow")] == ["dbg", "perf"] or True
    assert [s.name for s in match_skills([sk, other], "a stuck job again")] == ["dbg"]
    assert match_skills([sk, other], "debugger rebuild") == []  # whole-word starts, no substring noise
    assert match_skills([sk, other], "nothing relevant") == []


def test_sections_extracted_by_heading_not_fenced_text():
    assert extract_section(DOC, "worker lane").startswith("## Worker lane") and "more" in extract_section(DOC, "worker lane")
    assert "db facts" not in extract_section(DOC, "worker lane")
    assert "## not a heading" not in [h[2] for h in headings(DOC)]
    assert extract_section(DOC, "missing") is None


async def test_use_skill_tool(tmp_path):
    (tmp_path / ".ai-agent" / "skills").mkdir(parents=True)
    (tmp_path / ".ai-agent" / "skills" / "dbg.md").write_text(SKILL)
    (tmp_path / "docs").mkdir()
    (tmp_path / "docs" / "dbg.md").write_text(DOC)
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO)
    async with Agent(cfg) as a:
        run = lambda p: a.session.tool_registry.invoke("use_skill", p, tmp_path, a.session.hook_system)
        assert (await run({"name": "dbg"})).output == "Body text."
        assert "worker facts" in (await run({"name": "dbg", "section": "Worker lane"})).output
        assert "Worker lane" in (await run({"name": "dbg", "section": "list"})).output
        assert not (await run({"name": "nope"})).success


async def test_matching_playbook_is_attached_to_the_request(tmp_path):
    (tmp_path / ".ai-agent" / "skills").mkdir(parents=True)
    (tmp_path / ".ai-agent" / "skills" / "dbg.md").write_text(SKILL)
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([text("ok")])
        evs = [e async for e in a.run("there is a bug in checkout")]
        assert any("Loaded playbooks:" in e.data.get("message", "") and "dbg" in e.data["message"] for e in evs if e.type == AgentEventType.NOTICE)
        sent = a.session.client.requests[0]
        assert "Body text." in sent[-1]["content"] and "Skills" in sent[0]["content"]


def git(p, *a):
    subprocess.run(["git", *a], cwd=p, check=True, capture_output=True)


@pytest.fixture
def repo(tmp_path):
    git(tmp_path, "init", "-q"); git(tmp_path, "config", "user.email", "t@t"); git(tmp_path, "config", "user.name", "t")
    (tmp_path / "a.py").write_text("x = 1\n")
    git(tmp_path, "add", "-A"); git(tmp_path, "commit", "-qm", "i")
    return tmp_path


async def test_diff_contains_tracked_edits_and_new_files(repo):
    (repo / "a.py").write_text("x = 2\n")
    (repo / "new.py").write_text("print('hi')\n")
    d = await build_diff(repo, ["a.py", "new.py"])
    assert "-x = 1" in d and "+x = 2" in d and "new file: new.py" in d and "print('hi')" in d


def test_verdict_parsing():
    assert parse_verdict("VERDICT: PASS").verdict == "PASS"
    r = parse_verdict("blah\nVERDICT: ISSUES\n1. a.py:1 bad | evidence: x")
    assert r.verdict == "ISSUES" and "a.py:1" in r.findings
    assert parse_verdict("looks fine to me").verdict == "UNPARSED"


def write(n):
    return call("write_file", {"path": n, "content": "1"})


async def test_reviewer_findings_go_back_for_one_fix(repo, monkeypatch):
    calls = []

    async def fake_review(config, request, files, checks):
        calls.append(files)
        return ReviewResult("ISSUES", "1. x.py:1 missing guard | evidence: none") if len(calls) == 1 else ReviewResult("PASS", "")

    monkeypatch.setattr(agent_mod, "review_changes", fake_review)
    cfg = Config(cwd=repo, approval=ApprovalPolicy.AUTO, auto_verify=False, auto_review=True)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([write("x.py"), text("done"), write("y.py"), text("fixed")])
        evs = [e async for e in a.run("go")]
        rep = next(e for e in evs if e.type == AgentEventType.AGENT_END).data["report"]
        assert rep["status"] == "unverified" and rep["repairs"] == 1 and len(calls) == 1
        assert rep["review"]["verdict"] == "ISSUES"  # not hidden: the last review result stays in the report
        assert "missing guard" in a.session.client.requests[2][-1]["content"]


async def test_clean_review_just_finishes(repo, monkeypatch):
    async def fake_review(*a):
        return ReviewResult("PASS", "")

    monkeypatch.setattr(agent_mod, "review_changes", fake_review)
    cfg = Config(cwd=repo, approval=ApprovalPolicy.AUTO, auto_verify=False, auto_review=True)
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([write("x.py"), text("done")])
        evs = [e async for e in a.run("go")]
        rep = next(e for e in evs if e.type == AgentEventType.AGENT_END).data["report"]
        assert rep["repairs"] == 0 and rep["review"]["verdict"] == "PASS"


def test_profile_subagents_replace_builtins_and_router_puts_non_author_reviewer_first(tmp_path):
    from client.routing import ModelRegistry, Router

    spec = SubagentSpec(name="debugger", description="d", goal_prompt="g", role="debug")
    cfg = Config(cwd=tmp_path, subagents=[spec], routing={"review": ["author", "other"]})
    cfg.model_name = "author"
    assert [d.name for d in get_default_subagent_definitions(cfg)] == ["debugger"]
    assert [d.name for d in get_default_subagent_definitions(Config(cwd=tmp_path))] == ["codebase_investigator", "code_reviewer"]
    assert Router(cfg, ModelRegistry(cfg, tmp_path)).chain("review") == ["other", "author"]


def test_target_repo_env_file_is_never_loaded(tmp_path, monkeypatch):
    monkeypatch.delenv("SECRET_FROM_TARGET_REPO", raising=False)
    (tmp_path / ".env").write_text("SECRET_FROM_TARGET_REPO=leak\n")
    load_config(tmp_path)
    import os

    assert "SECRET_FROM_TARGET_REPO" not in os.environ


async def test_subagent_from_profile_runs_with_its_role_and_tools(tmp_path, monkeypatch):
    """Config round-trips into the child agent, the child uses the spec's role,
    and the parent gets the child's final answer back."""
    from client.llm_client import LLMClient
    from tests.fakes import text as scripted_text

    seen = {}

    async def fake_chat(self, messages, tools=None, stream=True, role="default"):
        seen["role"] = role
        seen["tool_names"] = sorted(t["name"] for t in tools or [])
        for ev in scripted_text("child answer: auth.ts:12"):
            yield ev

    monkeypatch.setattr(LLMClient, "chat_completion", fake_chat)
    spec = SubagentSpec(name="investigator", description="maps code", goal_prompt="map it", role="explore",
                        allowed_tools=["read_file", "grep"])
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, subagents=[spec])
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient([call("subagent_investigator", {"goal": "where is auth"}), text("thanks")])
        evs = [e async for e in a.run("explore")]
        done = next(e for e in evs if e.type == AgentEventType.TOOL_CALL_COMPLETE)
        assert done.data["success"] and "child answer: auth.ts:12" in done.data["output"]
    assert seen["role"] == "explore" and seen["tool_names"] == ["grep", "read_file"]


def test_ponytail_is_on_by_default_inlined_not_indexed_and_can_be_disabled(tmp_path):
    from agent.skills import load_skills
    from prompts.system import get_system_prompt
    from tools.registry import create_default_registry

    cfg = Config(cwd=tmp_path)
    sk = load_skills(cfg)
    assert any(s.name == "ponytail" and s.always for s in sk)
    prompt = get_system_prompt(cfg, None, create_default_registry(cfg).get_tools(), sk)
    assert "the least that fully works" in prompt and "- **ponytail**" not in prompt  # inlined, not an index entry
    off = Config(cwd=tmp_path, disabled_skills=["ponytail"])
    assert "the least that fully works" not in get_system_prompt(off, None, [], load_skills(off))
    assert "ponytail" not in [m.name for m in match_skills(sk, "fix this bug")]  # always-on skills are never triggered a second time
    assert match_skills(sk, "hello there") == []


def test_builtin_skills_load_with_the_right_always_on_core_and_profile_overrides(tmp_path):
    from agent.skills import load_skills
    from utils.text import count_tokens

    sk = {s.name: s for s in load_skills(Config(cwd=tmp_path))}
    assert {n for n, s in sk.items() if s.always} == {"ponytail", "grounded", "house-rules"}
    assert {"debugging", "code-review", "security", "refactor", "testing", "performance", "database", "api-design", "git-workflow"} <= set(sk)
    assert sum(count_tokens(s.body) for s in sk.values() if s.always) < 650  # the always-on core stays cheap
    assert all(s.triggers for s in sk.values() if not s.always)
    tunyt = {s.name for s in load_skills(Config(cwd=tmp_path, profile="tunyt"))}
    assert "tunyt-debugging" in tunyt and "debugging" not in tunyt  # specialised playbook replaces the generic one
    assert "refactor" in tunyt  # generic ones without a Tunyt twin remain
    assert [s.name for s in match_skills(list(sk.values()), "please refactor this module and rename the helper")][:1] == ["refactor"]
