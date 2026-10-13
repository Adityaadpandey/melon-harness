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
