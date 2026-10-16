import asyncio
import json
import re
import os
import signal
import sys
import tempfile
from typing import Any
from config.config import Config, HookConfig, HookTrigger
from tools.base import ToolResult


class HookSystem:
    def __init__(self, config: Config):
        self.config = config
        self.hooks: list[HookConfig] = []
        if self.config.hooks_enabled:
            self.hooks = [hook for hook in self.config.hooks if hook.enabled]

    async def _run_hook(self, hook: HookConfig, env: dict[str, str]) -> tuple[int, str]:
        """Run one hook and return (exit_code, stderr). A hook that cannot run or
        times out counts as exit 1: reported nowhere loud, and it never blocks."""
        try:
            if hook.command:
                return await self._run_command(hook.command, hook.timeout_sec, env)
            else:
                with tempfile.NamedTemporaryFile(
                    mode="w", suffix=".sh", delete=False
                ) as f:
                    f.write("#!/bin/bash\n")
                    f.write(hook.script)
                    script_path = f.name
                try:
                    os.chmod(script_path, 0o755)
                    return await self._run_command(script_path, hook.timeout_sec, env)
                finally:
                    os.unlink(script_path)
        except Exception:
            return 1, ""

    def _matching(self, trigger: HookTrigger, tool_name: str | None = None):
        for hook in self.hooks:
            if hook.trigger != trigger:
                continue
            # matcher is a regex on the tool name, like Claude Code's: "edit|write_file"
            if hook.matcher and tool_name is not None and not re.fullmatch(hook.matcher, tool_name):
                continue
            yield hook

    async def _run_command(
        self,
        command: str,
        timeout: float,
        env: dict[str, str],
    ) -> tuple[int, str]:
        process = await asyncio.create_subprocess_shell(
            command,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=self.config.cwd,
            env=env,
            start_new_session=True,
        )

        try:
            _, err = await asyncio.wait_for(process.communicate(), timeout=timeout)
        except asyncio.TimeoutError:
            if sys.platform != "win32":
                os.killpg(os.getpgid(process.pid), signal.SIGKILL)
            else:
                process.kill()
            await process.wait()
            return 1, ""
        return process.returncode or 0, err.decode(errors="replace").strip()

    def _build_env(
        self,
        trigger: HookTrigger,
        tool_name: str | None = None,
        user_message: str | None = None,
        error: Exception | None = None,
    ) -> dict[str, str]:
        env = os.environ.copy()
        env["AI_AGENT_TRIGGER"] = trigger.value
        env["AI_AGENT_CWD"] = str(self.config.cwd)

        if tool_name:
            env["AI_AGENT_TOOL_NAME"] = tool_name

        if user_message:
            env["AI_AGENT_USER_MESSAGE"] = user_message

        if error:
            env["AI_AGENT_ERROR"] = str(error)

        return env

    async def trigger_before_agent(self, user_message: str) -> None:
        env = self._build_env(
            HookTrigger.BEFORE_AGENT,
            user_message=user_message,
        )

        for hook in self.hooks:
            if hook.trigger == HookTrigger.BEFORE_AGENT:
                await self._run_hook(hook, env)

    async def trigger_after_agent(
        self,
        user_message: str,
        agent_response: str,
    ) -> None:
        env = self._build_env(
            HookTrigger.AFTER_AGENT,
            user_message=user_message,
        )
        env["AI_AGENT_RESPONSE"] = agent_response

        for hook in self.hooks:
            if hook.trigger == HookTrigger.AFTER_AGENT:
                await self._run_hook(hook, env)

    async def trigger_before_tool(
        self,
        tool_name: str,
        tool_params: dict[str, Any],
    ) -> str | None:
        """Exit code 2 from a before_tool hook blocks the call; its stderr is the
        reason the model sees (e.g. "run the formatter first"). Any other exit code
        lets the call go ahead."""
        env = self._build_env(HookTrigger.BEFORE_TOOL, tool_name=tool_name)
        env["AI_AGENT_TOOL_PARAMS"] = json.dumps(tool_params)

        for hook in self._matching(HookTrigger.BEFORE_TOOL, tool_name):
            code, err = await self._run_hook(hook, env)
            if code == 2:
                return f"Blocked by hook '{hook.name}': {err or 'no reason given'}"
        return None

    async def trigger_after_tool(
        self,
        tool_name: str,
        tool_params: dict[str, Any],
        tool_result: ToolResult,
    ) -> str | None:
        """Exit code 2 from an after_tool hook (a linter that found problems) hands
        its stderr back to the model next to the tool's own output."""
        env = self._build_env(HookTrigger.AFTER_TOOL, tool_name=tool_name)
        env["AI_AGENT_TOOL_PARAMS"] = json.dumps(tool_params)
        env["AI_AGENT_TOOL_RESULT"] = tool_result.to_model_output()

        feedback = []
        for hook in self._matching(HookTrigger.AFTER_TOOL, tool_name):
            code, err = await self._run_hook(hook, env)
            if code == 2 and err:
                feedback.append(f"[hook '{hook.name}']: {err}")
        return "\n".join(feedback) or None

    async def fire(self, trigger: HookTrigger, **info: str) -> None:
        """Events with no veto (compaction, errors): run every matching hook, ignore the exit code.
        `info` becomes AI_AGENT_<KEY> env vars, e.g. fire(PRE_COMPACT, tokens="180000")."""
        env = self._build_env(trigger)
        env.update({f"AI_AGENT_{k.upper()}": str(v) for k, v in info.items()})
        for hook in self._matching(trigger):
            await self._run_hook(hook, env)

    async def trigger_stop(self, user_message: str, agent_response: str) -> str | None:
        """Exit code 2 from a stop hook means "not done yet": its stderr goes back to the model as the next
        instruction. Example: a hook that runs the tests and exits 2 with "3 tests still fail" keeps the agent
        going until they pass. The caller caps how many times this can happen."""
        env = self._build_env(HookTrigger.STOP, user_message=user_message)
        env["AI_AGENT_RESPONSE"] = agent_response or ""
        for hook in self._matching(HookTrigger.STOP):
            code, err = await self._run_hook(hook, env)
            if code == 2:
                return f"[stop hook '{hook.name}']: {err or 'not finished yet'}"
        return None
