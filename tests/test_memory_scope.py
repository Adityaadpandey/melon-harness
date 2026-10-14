import asyncio

from config.config import Config
from tools.base import ToolInvocation
from tools.builtin.memory import MemoryTool


def run(cwd, **params):
    inv = ToolInvocation(params=params, cwd=cwd)
    return asyncio.run(MemoryTool(Config(cwd=str(cwd))).execute(inv)).output


def test_memory_is_per_workspace(tmp_path, monkeypatch):
    monkeypatch.setattr("tools.builtin.memory.get_data_dir", lambda: tmp_path / "data")
    a, b = tmp_path / "shop", tmp_path / "blog"
    a.mkdir(); b.mkdir()
    run(a, action="set", key="test_cmd", value="npm test")
    assert "npm test" in run(a, action="get", key="test_cmd")
    assert "not found" in run(b, action="get", key="test_cmd")
    assert "No memories" in run(b, action="list")
