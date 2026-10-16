"""Stop hooks can keep the agent working; session-start and subagent-stop hooks just run."""
import asyncio

from config.config import Config, HookConfig, HookTrigger
from hooks.hook_system import HookSystem


def system(tmp_path, trigger, command):
    cfg = Config(cwd=tmp_path, hooks_enabled=True, hooks=[HookConfig(name="h", trigger=trigger, command=command)])
    return HookSystem(cfg)


def test_stop_hook_exit_2_holds_the_agent_back(tmp_path):
    hs = system(tmp_path, HookTrigger.STOP, "echo 3 tests still fail >&2; exit 2")
    assert "3 tests still fail" in asyncio.run(hs.trigger_stop("fix it", "done"))


def test_stop_hook_exit_0_lets_it_finish(tmp_path):
    assert asyncio.run(system(tmp_path, HookTrigger.STOP, "exit 0").trigger_stop("x", "y")) is None


def test_session_start_hook_runs_with_its_info(tmp_path):
    out = tmp_path / "seen"
    hs = system(tmp_path, HookTrigger.SESSION_START, f'echo "$AI_AGENT_SESSION_ID" > {out}')
    asyncio.run(hs.fire(HookTrigger.SESSION_START, session_id="abc"))
    assert out.read_text().strip() == "abc"
