import pytest

from safety.approval import is_dangerous_command


@pytest.mark.parametrize("cmd", [
    "rm -rf .git", "rm -r -f ./.git/", "rm .git/index", "git reset --hard HEAD~1",
    "git clean -fdx", "git push --force origin main", "git push origin main -f",
    "git checkout .", "git checkout -- .", "git restore .", "git branch -D feature",
])
def test_history_destroying_commands_are_blocked(cmd):
    assert is_dangerous_command(cmd)


@pytest.mark.parametrize("cmd", [
    "git status", "git checkout -b feat", "git checkout main", "git restore src/a.py",
    "rm -rf build", "rm notes.gitignore.bak", "git push origin feat", "git branch -d done",
])
def test_normal_commands_are_not_flagged(cmd):
    assert not is_dangerous_command(cmd)


async def test_overwriting_existing_file_in_workspace_is_approved_under_auto(tmp_path):
    from config.config import ApprovalPolicy
    from safety.approval import ApprovalContext, ApprovalDecision, ApprovalManager

    f = tmp_path / "a.py"
    ctx = ApprovalContext("write_file", {}, True, [f], is_dangerous=True)
    assert await ApprovalManager(ApprovalPolicy.AUTO, tmp_path).check_approval(ctx) == ApprovalDecision.APPROVED
    assert await ApprovalManager(ApprovalPolicy.ON_REQUEST, tmp_path).check_approval(ctx) == ApprovalDecision.NEEDS_CONFIRMATION
    outside = ApprovalContext("write_file", {}, True, [tmp_path.parent / "x.py"], is_dangerous=True)
    assert await ApprovalManager(ApprovalPolicy.AUTO, tmp_path).check_approval(outside) == ApprovalDecision.NEEDS_CONFIRMATION


from agent.agent import Agent
from config.config import ApprovalPolicy, Config
from safety.approval import ApprovalContext, ApprovalDecision, ApprovalManager, Decision, command_key
from tests.fakes import ScriptedClient, call, text


def edit_ctx(path):
    return ApprovalContext("edit", {}, True, [path], tool_kind="write")


async def test_edits_ask_by_default_and_auto_edit_skips_the_question(tmp_path):
    f = tmp_path / "a.py"
    assert await ApprovalManager(ApprovalPolicy.ON_REQUEST, tmp_path).check_approval(edit_ctx(f)) == ApprovalDecision.NEEDS_CONFIRMATION
    assert await ApprovalManager(ApprovalPolicy.AUTO_EDIT, tmp_path).check_approval(edit_ctx(f)) == ApprovalDecision.APPROVED
    assert await ApprovalManager(ApprovalPolicy.NEVER, tmp_path).check_approval(edit_ctx(f)) == ApprovalDecision.REJECTED
    out = tmp_path.parent / "elsewhere.py"
    assert await ApprovalManager(ApprovalPolicy.AUTO, tmp_path).check_approval(edit_ctx(out)) == ApprovalDecision.NEEDS_CONFIRMATION
    assert await ApprovalManager(ApprovalPolicy.YOLO, tmp_path).check_approval(edit_ctx(out)) == ApprovalDecision.APPROVED


def test_command_keys_are_specific_enough():
    assert command_key("git push origin main") == "git push"
    assert command_key("pytest -q tests") == "pytest"
    assert command_key("pnpm test:unit") == "pnpm test:unit"


async def test_dont_ask_again_never_whitelists_dangerous_commands(tmp_path):
    m = ApprovalManager(ApprovalPolicy.ON_REQUEST, tmp_path)
    m.always_commands.add("git reset")
    ctx = ApprovalContext("shell", {}, True, [], command="git reset --hard", tool_kind="shell")
    assert await m.check_approval(ctx) == ApprovalDecision.REJECTED


async def run_agent(tmp_path, callback, script):
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.ON_REQUEST, auto_verify=False)
    async with Agent(cfg, callback) as a:
        a.session.client = ScriptedClient(script)
        evs = [e async for e in a.run("go")]
        # the session is torn down on exit, so snapshot what the tests inspect
        snap = type("Snap", (), {})()
        snap.requests = a.session.client.requests
        snap.policy = a.session.approval_manager.approval_policy
        snap.always = set(a.session.approval_manager.always_commands)
        return snap, evs


def write(n, c="1"):
    return call("write_file", {"path": n, "content": c}, f"c-{n}")


async def test_yes_applies_the_edit_and_async_callbacks_work(tmp_path):
    async def yes(conf):
        assert conf.diff is not None and "+1" in conf.diff.to_diff()
        return Decision(True)

    await run_agent(tmp_path, yes, [write("a.py"), text("done")])
    assert (tmp_path / "a.py").read_text() == "1"


async def test_no_with_feedback_reaches_the_model_and_nothing_is_written(tmp_path):
    a, evs = await run_agent(tmp_path, lambda c: Decision(False, feedback="Use a class instead."), [write("a.py"), text("ok")])
    assert not (tmp_path / "a.py").exists()
    tool_msg = next(m for m in a.requests[1] if m["role"] == "tool")
    assert "rejected" in tool_msg["content"] and "Use a class instead." in tool_msg["content"]


async def test_always_switches_to_auto_edit_so_later_edits_dont_ask(tmp_path):
    asked = []

    def cb(conf):
        asked.append(conf.tool_name)
        return Decision(True, always=True)

    a, _ = await run_agent(tmp_path, cb, [write("a.py"), write("b.py"), text("done")])
    assert asked == ["write_file"] and (tmp_path / "b.py").exists()
    assert a.policy == ApprovalPolicy.AUTO_EDIT


async def test_shell_always_remembers_the_command_not_all_edits(tmp_path):
    asked = []

    def cb(conf):
        asked.append(conf.command)
        return Decision(True, always=True)

    a, _ = await run_agent(
        tmp_path, cb,
        [call("shell", {"command": "touch a"}, "1"), call("shell", {"command": "touch b"}, "2"), write("x.py"), text("done")],
    )
    assert asked == ["touch a", None]  # "touch b" was covered by the earlier choice; the edit still asks
    assert a.always == {"touch"} and a.policy == ApprovalPolicy.AUTO_EDIT


async def test_non_interactive_message_says_how_to_proceed(tmp_path):
    a, evs = await run_agent(tmp_path, None, [write("a.py"), text("ok")])
    tool_msg = next(m for m in a.requests[1] if m["role"] == "tool")
    assert "--approval auto-edit" in tool_msg["content"]
