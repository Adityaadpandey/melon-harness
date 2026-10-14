"""Three small repos with different stacks, used to prove one project's facts, memory and
files never leak into another's session."""
import asyncio

import pytest

from agent.learn import Learned
from config.config import ApprovalPolicy, Config
from hooks.hook_system import HookSystem
from prompts.system import get_system_prompt
from tools.registry import create_default_registry

STACKS = {
    "webapp": {"package.json": '{"scripts": {"test": "vitest"}}', "src/App.tsx": "export {}"},
    "service": {"pyproject.toml": "[tool.pytest]", "app/main.py": "print(1)"},
    "gocli": {"go.mod": "module gocli", "main.go": "package main"},
}


@pytest.fixture
def repos(tmp_path):
    out = {}
    for name, files in STACKS.items():
        root = tmp_path / name
        for rel, body in files.items():
            (root / rel).parent.mkdir(parents=True, exist_ok=True)
            (root / rel).write_text(body)
        out[name] = root
    return out


def test_learned_lessons_stay_in_their_repo(repos, tmp_path):
    base = tmp_path / "data"
    Learned(repos["webapp"], base).add_lesson("always run vitest before finishing a change", "user")
    for other in ("service", "gocli"):
        c = Config(cwd=str(repos[other]))
        assert "vitest" not in Learned(c.cwd, base).prompt_section()
    assert "vitest" in Learned(repos["webapp"], base).prompt_section()


def test_prompt_names_only_the_active_workspace(repos):
    for name, root in repos.items():
        c = Config(cwd=str(root))
        prompt = get_system_prompt(c, None, create_default_registry(c).get_tools(), [])
        assert str(root) in prompt
        assert all(str(r) not in prompt for n, r in repos.items() if n != name)


def test_tools_cannot_write_into_a_sibling_repo(repos):
    c = Config(cwd=str(repos["webapp"]), approval=ApprovalPolicy.YOLO)
    reg = create_default_registry(c)
    target = repos["service"] / "app" / "main.py"
    res = asyncio.run(reg.invoke("write_file", {"path": str(target), "content": "pwned"}, repos["webapp"], HookSystem(c)))
    assert not res.success and target.read_text() == "print(1)"
