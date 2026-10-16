"""A deny rule stops the call before anything runs; an allow rule skips the prompt but not the safety refusals."""
import asyncio

from config.config import ApprovalPolicy, Config
from hooks.hook_system import HookSystem
from safety.approval import ApprovalManager
from safety.rules import save_rule
from tools.registry import create_default_registry


def run(tmp_path, tool, params, policy=ApprovalPolicy.ON_REQUEST):
    cfg = Config(cwd=tmp_path, sandbox="off")
    reg = create_default_registry(cfg)
    mgr = ApprovalManager(policy, tmp_path)  # no callback: anything that would prompt fails closed
    return asyncio.run(reg.invoke(tool, params, tmp_path, HookSystem(cfg), mgr))


def test_deny_blocks_a_read(tmp_path):
    (tmp_path / "notes.txt").write_text("hi")
    save_rule(tmp_path, "deny", "Read(notes*)")
    r = run(tmp_path, "read_file", {"path": "notes.txt"})
    assert not r.success and "permission rule" in r.error


def test_allow_skips_the_prompt(tmp_path):
    assert not run(tmp_path, "shell", {"command": "touch a.txt"}).success  # no one to ask: fails closed
    save_rule(tmp_path, "allow", "Bash(touch:*)")
    assert run(tmp_path, "shell", {"command": "touch a.txt"}).success and (tmp_path / "a.txt").exists()


def test_allow_does_not_lift_plan_mode(tmp_path):
    save_rule(tmp_path, "allow", "Bash(touch:*)")
    r = run(tmp_path, "shell", {"command": "touch b.txt"}, ApprovalPolicy.PLAN)
    assert not r.success and not (tmp_path / "b.txt").exists()
