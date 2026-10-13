import asyncio
import os
from pathlib import Path
import signal
import sys
from tools.base import Tool, ToolConfirmation, ToolInvocation, ToolKind, ToolResult
from pydantic import BaseModel, Field
import fnmatch
import tempfile
import time

BLOCKED_COMMANDS = {
    "rm -rf /",
    "rm -rf ~",
    "rm -rf /*",
    "dd if=/dev/zero",
    "dd if=/dev/random",
    "mkfs",
    "fdisk",
    "parted",
    ":(){ :|:& };:",  # Fork bomb
    "chmod 777 /",
    "chmod -R 777",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "init 0",
    "init 6",
}


class BackgroundJob:
    """A command running detached, e.g. `pnpm build`. Output goes to a log file the model can tail."""

    def __init__(self, job_id: int, command: str, log: Path, process):
        self.id, self.command, self.log, self.process = job_id, command, log, process
        self.started = time.monotonic()
        self.announced = False

    @property
    def done(self) -> bool:
        return self.process.returncode is not None

    def tail(self, lines: int = 15) -> str:
        text = self.log.read_text(errors="replace") if self.log.exists() else ""
        return "\n".join(text.rstrip().splitlines()[-lines:])


JOBS: dict[int, BackgroundJob] = {}


def finished_jobs() -> list[BackgroundJob]:
    """Jobs that ended since the last call, each returned once."""
    out = [j for j in JOBS.values() if j.done and not j.announced]
    for j in out:
        j.announced = True
    return out


def kill_job(j: BackgroundJob) -> None:
    if j.done:
        return
    try:
        if sys.platform != "win32":
            os.killpg(os.getpgid(j.process.pid), signal.SIGKILL)
        else:
            j.process.kill()
    except ProcessLookupError:
        pass


def kill_jobs() -> None:
    for j in JOBS.values():
        kill_job(j)


class ShellParams(BaseModel):
    command: str = Field(..., description="The shell command to execute")
    timeout: int = Field(
        120, ge=1, le=600, description="Timeout in seconds (default: 120)"
    )
    cwd: str | None = Field(None, description="Working directory for the command")
    background: bool = Field(
        False,
        description=(
            "Run detached and return at once with a log path. Use for anything slow "
            "(builds, installs, dev servers, long test suites) so you can keep working; "
            "you are told when it finishes. Timeout does not apply."
        ),
    )


class ShellTool(Tool):
    name = "shell"
    kind = ToolKind.SHELL
    description = "Execute a shell command. Use this for running system commands, scripts and CLI tools."

    schema = ShellParams

    async def get_confirmation(
        self, invocation: ToolInvocation
    ) -> ToolConfirmation | None:
        params = ShellParams(**invocation.params)

        for blocked in BLOCKED_COMMANDS:
            if blocked in params.command:
                return ToolConfirmation(
                    tool_name=self.name,
                    params=invocation.params,
                    description=f"Execute (BLOCKED): {params.command}",
                    command=params.command,
                    is_dangerous=True,
                )

        return ToolConfirmation(
            tool_name=self.name,
            params=invocation.params,
            description=f"Execute: {params.command}",
            command=params.command,
            is_dangerous=False,
        )

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        params = ShellParams(**invocation.params)

        command = params.command.lower().strip()
        for blocked in BLOCKED_COMMANDS:
            if blocked in command:
                return ToolResult.error_result(
                    f"Command blocked for safety: {params.command}",
                    metadata={"blocked": True},
                )

        if params.cwd:
            cwd = Path(params.cwd)
            if not cwd.is_absolute():
                cwd = invocation.cwd / cwd
        else:
            cwd = invocation.cwd

        if not cwd.exists():
            return ToolResult.error_result(f"Working directory doesn't exist: {cwd}")

        env = self._build_environment()
        if sys.platform == "win32":
            shell_cmd = ["cmd.exe", "/c", params.command]
        else:
            shell_cmd = ["/bin/bash", "-c", params.command]

        from safety.sandbox import wrap

        if params.background:
            job_id = len(JOBS) + 1
            log = Path(tempfile.gettempdir()) / f"mew-job-{os.getpid()}-{job_id}.log"
            with open(log, "wb") as out:
                process = await asyncio.create_subprocess_exec(
                    *wrap(shell_cmd, self.config),
                    stdout=out,
                    stderr=asyncio.subprocess.STDOUT,
                    stdin=asyncio.subprocess.DEVNULL,
                    cwd=cwd,
                    env=env,
                    start_new_session=True,
                )
            JOBS[job_id] = BackgroundJob(job_id, params.command, log, process)
            # reap it so returncode gets set even if nobody awaits the job
            asyncio.get_running_loop().create_task(process.wait())
            return ToolResult.success_result(
                f"Started background job {job_id}: {params.command}\n"
                f"Output: {log} (read it with `tail`). Stop it with `kill -- -{process.pid}`. "
                "You will be told when it finishes; keep working on other things meanwhile."
            )

        process = await asyncio.create_subprocess_exec(
            *wrap(shell_cmd, self.config),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=cwd,
            env=env,
            start_new_session=True,
        )

        try:
            stdout_data, stderr_data = await asyncio.wait_for(
                process.communicate(),
                timeout=params.timeout,
            )
        except (asyncio.TimeoutError, asyncio.CancelledError) as exc:
            # on timeout or user interrupt, never leave the process group running
            if sys.platform != "win32":
                os.killpg(os.getpgid(process.pid), signal.SIGKILL)
            else:
                process.kill()
            await process.wait()
            if isinstance(exc, asyncio.CancelledError):
                raise
            return ToolResult.error_result(f"Command timed out after {params.timeout}s")

        stdout = stdout_data.decode("utf-8", errors="replace")
        stderr = stderr_data.decode("utf-8", errors="replace")
        exit_code = process.returncode

        output = ""
        if stdout.strip():
            output += stdout.rstrip()

        if stderr.strip():
            output += "\n--- stderr ---\n"
            output += stderr.rstrip()

        if exit_code != 0:
            output += f"\nExit code: {exit_code}"

        # the registry applies the model-facing cap; this only bounds memory
        if len(output) > 400 * 1024:
            output = output[: 400 * 1024] + "\n... [output truncated]"

        return ToolResult(
            success=exit_code == 0,
            output=output,
            error=stderr if exit_code != 0 else None,
            exit_code=exit_code,
        )

    def _build_environment(self) -> dict[str, str]:
        return build_environment(self.config)


def build_environment(config) -> dict[str, str]:
    """Process env with secret-looking variables (*KEY*, *TOKEN*, ...) removed."""
    env = os.environ.copy()
    policy = config.shell_environment

    if not policy.ignore_default_excludes:
        for pattern in policy.exclude_patterns:
            for k in [k for k in env if fnmatch.fnmatch(k.upper(), pattern.upper())]:
                del env[k]

    if policy.set_vars:
        env.update(policy.set_vars)

    return env
