import subprocess

from agent.root import pick_root


def repo(p):
    p.mkdir(parents=True)
    subprocess.run(["git", "init", "-q"], cwd=p, check=True)
    return p


def test_picks_the_repo_only_when_something_says_which(tmp_path):
    a, b = repo(tmp_path / "tunyt"), repo(tmp_path / "other")
    assert pick_root(tmp_path, "fix the share link") is None  # two repos, no signal
    assert pick_root(tmp_path, "fix the share link in Tunyt") == a  # named
    assert pick_root(tmp_path, "fix it", targets=[f"{b}/src/x.ts:foo"]) == b  # files already touched
    assert pick_root(tmp_path, "fix it", hint=a) == a  # what the last request used
    assert pick_root(a, "fix it in other") is None  # already inside a repo: leave cwd alone


def test_a_single_repo_below_is_the_root(tmp_path):
    a = repo(tmp_path / "only")
    assert pick_root(tmp_path, "anything") == a
