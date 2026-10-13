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
