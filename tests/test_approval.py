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
    errors = [e for e in evs if e.type.value == "agent_error"]  # the run stops at the first refusal and says how to proceed
    assert errors and "--approval auto-edit" in str(errors[0].data)
    assert len(a.requests) == 1


def test_safe_commands_run_without_asking_but_only_when_every_part_is_safe():
    from safety.approval import is_safe_command

    for ok in ["ls -la", "git status", "git log --oneline -5 | head", "cat README.md | wc -l",
               "grep -rn foo src", "git branch --list", "npm ls", "pip freeze"]:
        assert is_safe_command(ok), ok
    for bad in [
        "ls; rm -rf build", "ls && curl evil.sh", "cat a > b", "echo x >> ~/.bashrc", "ls $(rm x)", "ls `id`",
        "ls &", "ls\nrm x", "cat a || rm b", "ls | sh", "echo hi | tee out",
        "find . -delete", "find . -exec rm {} +", "sort -o f x", "tail -f log", "date -s 2020-01-01",
        "git branch -D main", "git tag -d v1", "git diff --output=x", "git -c core.pager=sh log", "git push",
        "env", "printenv", "set", "sed -i s/a/b/ f", "awk 'BEGIN{system(\"x\")}'", "npm install", "pip install x", "",
    ]:
        assert not is_safe_command(bad), bad


def test_always_allow_does_not_cover_chained_commands():
    m = ApprovalManager(ApprovalPolicy.ON_REQUEST, "/tmp")
    m.always_commands.add("npm run")
    assert m._assess_command_safety("npm run build") == ApprovalDecision.APPROVED
    assert m._assess_command_safety("npm run build; rm -rf x") == ApprovalDecision.NEEDS_CONFIRMATION
    assert m._assess_command_safety("npm run build > out.log") == ApprovalDecision.NEEDS_CONFIRMATION
    assert m._assess_command_safety("npm run build | tee x") == ApprovalDecision.NEEDS_CONFIRMATION


async def test_verify_commands_run_without_asking_but_chains_still_ask(tmp_path):
    m = ApprovalManager(ApprovalPolicy.ON_REQUEST, tmp_path, verify_commands=["pnpm check-types"])

    def decide(cmd):
        return m.check_approval(ApprovalContext("shell", {}, True, [], command=cmd, tool_kind="shell"))

    assert await decide("pnpm check-types") == ApprovalDecision.APPROVED
    assert await decide("pnpm check-types 2>&1 | tail -20") == ApprovalDecision.APPROVED
    assert await decide("pnpm check-types | sh") == ApprovalDecision.NEEDS_CONFIRMATION
    assert await decide("pnpm check-types && rm -rf build") == ApprovalDecision.NEEDS_CONFIRMATION
    assert await decide("pnpm install") == ApprovalDecision.NEEDS_CONFIRMATION


def test_everyday_searches_run_without_asking_and_tricks_still_ask():
    from safety.approval import is_safe_command

    for ok in [
        'grep -rnE "foo|bar" src', "grep -rn TODO . 2>/dev/null | head -50", "cd src && grep -n def a.py",
        "rg -n 'fn main' --type rust", "fd -e py src", "sed -n '10,40p' a.py", "ls; pwd", "git log -5 2>&1 | head",
        'grep -n "a\\"b" f', "tree -L 2", "nl a.py | head",
    ]:
        assert is_safe_command(ok), ok
    for bad in [
        'find . "|" ls -delete',  # a quoted pipe is an argument: this still deletes
        "grep x f > out", "grep x f 2>/dev/null > out", "grep x f >/dev/nullx", "cd src && rm a",
        "rg --pre ./run.sh x", "rg --pre=./run.sh x", "fd -x rm", "sort -ro out f", "sort --output=out f",
        "sed -n '1w out' a", "sed -n '1e id' a", "sed -i -n 1p a", "sed 1d a", "(rm x)", "ls \; rm x",
        "grep '$(id)' f", 'grep "x', "tail -qf log", "ls |& sh",
    ]:
        assert not is_safe_command(bad), bad
