"""A change that breaks a file is reported in the edit's own result, and only what the change broke."""
import asyncio
import shutil
from pathlib import Path

import pytest

from config.config import ApprovalPolicy, Config
from utils.diagnostics import new_problems


def found(name, old, new, cwd=Path(".")):
    return asyncio.run(new_problems(Path(name), old, new, cwd))


def test_a_new_syntax_error_is_reported_but_an_old_one_is_not():
    assert "SyntaxError" in found("a.py", "x = 1\n", "x = (1\n")[0]
    broken = "def f(:\n    pass\n"
    assert found("a.py", broken, broken + "y = 2\n") == []  # it was already broken; not this edit's doing


def test_data_files_are_parsed():
    assert found("c.json", '{"a": 1}', '{"a": 1,}')
    assert found("c.toml", "a = 1\n", "a = \n") and found("c.toml", "a = 1\n", "a = 2\n") == []


@pytest.mark.skipif(not shutil.which("node"), reason="node not installed")
def test_javascript_syntax_errors_come_from_node():
    assert found("x.mjs", "export const a = 1\n", "export const a = (1\n")
    assert found("x.js", "import x from 'y'\n", "import x from 'y'\nx()\n") == []  # module syntax is fine


async def test_a_broken_edit_says_so_in_its_result(tmp_path):
    from agent.agent import Agent
    from tests.fakes import ScriptedClient, call, text

    (tmp_path / "m.py").write_text("def f():\n    return 1\n")
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO)
    async with Agent(cfg) as agent:
        agent.session.client = ScriptedClient([
            call("read_file", {"path": "m.py"}),
            call("edit", {"path": "m.py", "old_string": "return 1", "new_string": "return (1"}, "c2"),
            text("done"),
        ])
        [e async for e in agent.run("change it")]
        results = [m["content"] for m in agent.session.client.requests[-1] if m["role"] == "tool"]
        assert any("<diagnostics" in r and "SyntaxError" in r for r in results)
