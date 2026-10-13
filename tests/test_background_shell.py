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
