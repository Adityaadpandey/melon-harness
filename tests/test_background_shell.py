import asyncio
from pathlib import Path

from config.config import Config
from tools.base import ToolInvocation
from tools.builtin.shell import JOBS, ShellTool, finished_jobs


def test_background_job_returns_fast_then_reports_done():
    async def go():
        tool = ShellTool(Config())
        res = await tool.execute(ToolInvocation(
            cwd=Path("."), params={"command": "sleep 0.3; echo built", "background": True}
        ))
        assert res.success and "background job" in res.output
        assert finished_jobs() == []  # still running
        await asyncio.sleep(0.8)
        done = finished_jobs()
        assert len(done) == 1 and "built" in done[0].tail()
        assert finished_jobs() == []  # announced only once
    asyncio.run(go())
    JOBS.clear()


def test_slow_foreground_command_moves_to_background_on_its_own(monkeypatch):
    import tools.builtin.shell as sh

    monkeypatch.setattr(sh, "AUTO_BACKGROUND_AFTER", 0.3)

    async def go():
        tool = ShellTool(Config())
        quick = await tool.execute(ToolInvocation(cwd=Path("."), params={"command": "echo hi"}))
        assert quick.output == "hi" and "background_job" not in quick.metadata
        res = await tool.execute(ToolInvocation(
            cwd=Path("."), params={"command": "echo early; sleep 1; echo late", "timeout": 30}
        ))
        assert "background_job" in res.metadata
        await asyncio.sleep(1.5)
        done = finished_jobs()
        assert len(done) == 1 and "early" in done[0].tail() and "late" in done[0].tail()
    asyncio.run(go())
    JOBS.clear()


def test_exit_codes_are_explained_and_grep_finding_nothing_is_not_a_failure(tmp_path):
    from tools.builtin.shell import exit_meaning

    (tmp_path / "a.txt").write_text("hello\n")

    def sh(cmd):
        return asyncio.run(ShellTool(Config(cwd=tmp_path)).execute(ToolInvocation(cwd=tmp_path, params={"command": cmd})))

    nothing = sh("grep -n nope a.txt")
    assert nothing.success and "no matches found" in nothing.output
    assert sh("true").output == "(command succeeded with no output)"
    assert not sh("exit 3").success
    assert exit_meaning("pytest -x", 1) is None  # a failing test run stays a failure
    assert exit_meaning("make", 137) == "killed by signal 9, often out of memory"
