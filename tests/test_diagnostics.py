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


def _typescript_install() -> Path | None:
    """Any TypeScript on this machine to borrow (tests never install one)."""
    for root in (Path.home() / "Devlopment" / "tunyt", Path(__file__).parents[1] / "pi"):
        if (root / "node_modules" / "typescript" / "lib" / "tsserver.js").is_file():
            return root / "node_modules" / "typescript"
    return None


@pytest.mark.skipif(not (shutil.which("node") and _typescript_install()), reason="no TypeScript install to borrow")
def test_typescript_errors_come_from_the_projects_own_tsserver(tmp_path):
    (tmp_path / "node_modules").mkdir()
    (tmp_path / "node_modules" / "typescript").symlink_to(_typescript_install())
    (tmp_path / "tsconfig.json").write_text('{"compilerOptions": {"strict": true, "noEmit": true}}')
    src = tmp_path / "src" / "a.ts"
    src.parent.mkdir()
    old = "export function total(xs: number[]): number {\n  return xs.reduce((a, b) => a + b, 0)\n}\n"
    new = old.replace("return xs", "const label: string = xs.length\n  return xs")
    src.write_text(new)

    async def go():
        broke = await new_problems(src, old, new, tmp_path)
        same = await new_problems(src, new, new + "\n", tmp_path)  # the error was already there: not this edit's
        return broke, same

    broke, same = asyncio.run(go())
    assert len(broke) == 1 and "TS2322" in broke[0] and broke[0].startswith("2:")
    assert same == []


@pytest.mark.skipif(not (shutil.which("node") and _typescript_install()), reason="no TypeScript install to borrow")
def test_a_project_still_loading_skips_one_check_and_answers_the_next(tmp_path, monkeypatch):
    import utils.diagnostics as diag

    (tmp_path / "node_modules").mkdir()
    (tmp_path / "node_modules" / "typescript").symlink_to(_typescript_install())
    src = tmp_path / "a.ts"
    src.write_text("const n: number = 'x'\n")

    async def go():
        monkeypatch.setattr(diag, "TS_TIMEOUT", 0.001)  # a monorepo taking longer than the wait
        first = await diag.problems(src, src.read_text(), tmp_path)
        monkeypatch.setattr(diag, "TS_TIMEOUT", 20.0)
        return first, await diag.problems(src, src.read_text(), tmp_path)

    first, second = asyncio.run(go())
    assert first == [] and any("TS2322" in p for p in second)  # same server, still in step with its replies
