from pathlib import Path

from agent.verify import select_checks
from config.config import ApprovalPolicy, Config, VerifyRule
from config.loader import _instructions_for, load_config
from safety.guard import violation

RULES = dict(
    forbidden_paths=[".env", ".env.*", "*.pem"],
    forbidden_commands=[r"\bpsql\b", r"prisma\s+(studio|migrate|db)\b"],
)


def cfg(tmp_path, **kw):
    return Config(cwd=tmp_path, **{**RULES, **kw})


def test_protected_files_blocked_for_every_tool_shape(tmp_path):
    c = cfg(tmp_path)
    assert violation("read_file", {"path": ".env"}, c)
    assert violation("read_file", {"path": "apps/server/.env.production"}, c)
    assert violation("read_file", {"path": "certs/server.pem"}, c)
    assert violation("shell", {"command": "cat .env.local"}, c)
    assert violation("shell", {"command": "grep KEY ./apps/.env"}, c)
    assert violation("shell", {"command": "source<.env"}, c)
    assert violation("read_file", {"path": "src/environment.ts"}, c) is None


def test_database_commands_blocked(tmp_path):
    c = cfg(tmp_path)
    for cmd in ["psql -h prod", "npx prisma migrate dev", "pnpm prisma studio", "PSQL x"]:
        assert violation("shell", {"command": cmd}, c), cmd
    assert violation("shell", {"command": "npx prisma generate"}, c) is None
    assert violation("shell", {"command": "npx tsc --noEmit"}, c) is None


def test_no_rules_means_no_blocking(tmp_path):
    assert violation("shell", {"command": "cat .env"}, Config(cwd=tmp_path, forbidden_paths=[], forbidden_commands=[])) is None


async def test_guard_applies_through_registry(tmp_path):
    from agent.agent import Agent
    from agent.events import AgentEventType
    from tests.fakes import ScriptedClient, call, text

    (tmp_path / ".env").write_text("API_KEY=sekret")
    c = cfg(tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False)
    async with Agent(c) as a:
        a.session.client = ScriptedClient([call("read_file", {"path": ".env"}), text("ok")])
        evs = [e async for e in a.run("read it")]
        done = next(e for e in evs if e.type == AgentEventType.TOOL_CALL_COMPLETE)
        assert not done.data["success"] and "sekret" not in str(done.data)


def test_verify_rules_select_by_changed_path_and_expand_files(tmp_path):
    c = cfg(
        tmp_path,
        verify_rules=[
            VerifyRule(paths=["apps/server/"], cwd="apps/server", commands=["tsc", "biome {files}"]),
            VerifyRule(paths=["apps/web/"], cwd="apps/web", commands=["web-check"]),
        ],
    )
    got = select_checks(c, ["apps/server/src/a b.ts", "README.md"])
    assert got == [("tsc", "apps/server"), ("biome 'src/a b.ts'", "apps/server")]
    assert select_checks(c, ["docs/x.md"]) == []


def test_large_instruction_files_become_an_outline(tmp_path):
    (tmp_path / ".git").mkdir()
    (tmp_path / "AGENTS.md").write_text("# Rules\n" + "filler line\n" * 800 + "## Server\nmore\n")
    out = _instructions_for(tmp_path)
    assert "line 1: # Rules" in out and "## Server" in out and "filler" not in out


def test_small_instruction_files_inline_and_stubs_skipped(tmp_path):
    (tmp_path / ".git").mkdir()
    (tmp_path / "AGENTS.md").write_text("Always run the linter before finishing. " * 3)
    (tmp_path / "CLAUDE.md").write_text("@AGENTS.md")
    out = _instructions_for(tmp_path)
    assert "Always run the linter" in out and "@AGENTS.md" not in out


def test_tunyt_profile_is_selected_by_path_and_project_config_wins(tmp_path):
    root = tmp_path / "tunyt" / "apps" / "server"
    (root / ".ai-agent").mkdir(parents=True)
    (root / ".ai-agent" / "config.toml").write_text("max_turns = 7\n")
    c = load_config(root)
    assert c.profile == "tunyt" and ".env" in c.forbidden_paths
    assert c.max_turns == 7
    assert "Tunyt profile" in c.developer_instructions
    other = tmp_path / "somewhere"
    other.mkdir()
    assert load_config(other).profile is None


async def test_symlinked_cwd_does_not_make_inside_edits_look_outside(tmp_path):
    from safety.approval import ApprovalContext, ApprovalDecision, ApprovalManager

    real = tmp_path / "real"
    real.mkdir()
    link = tmp_path / "link"
    link.symlink_to(real)
    m = ApprovalManager(ApprovalPolicy.AUTO, link)
    ctx = ApprovalContext("write_file", {}, True, [real / "a.py"])
    assert await m.check_approval(ctx) == ApprovalDecision.APPROVED
