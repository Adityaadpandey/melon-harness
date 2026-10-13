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
