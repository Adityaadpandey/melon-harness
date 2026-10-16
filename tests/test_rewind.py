"""Rewinding two requests restores the first one's starting point, deleting files either request created."""
import asyncio
import subprocess

from agent.verify import backup, undo


def git(d, *a):
    subprocess.run(["git", *a], cwd=d, check=True, capture_output=True)


def test_rewind_across_two_requests(tmp_path):
    git(tmp_path, "init", "-q")
    git(tmp_path, "config", "user.email", "t@t")
    git(tmp_path, "config", "user.name", "t")
    (tmp_path / "a.txt").write_text("v0")
    git(tmp_path, "add", ".")
    git(tmp_path, "commit", "-qm", "init")
    ref1 = asyncio.run(backup(tmp_path))
    (tmp_path / "a.txt").write_text("v1")
    (tmp_path / "new1.txt").write_text("x")
    asyncio.run(backup(tmp_path))  # request 2 starts here
    (tmp_path / "a.txt").write_text("v2")
    (tmp_path / "new2.txt").write_text("y")
    restored, deleted, skipped = asyncio.run(undo(tmp_path, ref1, ["a.txt", "new1.txt", "new2.txt"], ["new1.txt", "new2.txt"]))
    assert (tmp_path / "a.txt").read_text() == "v0"
    assert not (tmp_path / "new1.txt").exists() and not (tmp_path / "new2.txt").exists()
    assert sorted(deleted) == ["new1.txt", "new2.txt"] and not skipped
