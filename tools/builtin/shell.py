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


AUTO_BACKGROUND_AFTER = 15  # seconds a foreground command may run before it is moved to the background
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
            "Detach at once and return a log path. Not required: any command still running "
            "after ~15s is moved to the background automatically, and you are told when it finishes."
        ),
    )


class ShellTool(Tool):
    name = "shell"
    kind = ToolKind.SHELL
    description = (
        "Run a shell command (builds, tests, git, scripts). The working directory persists between calls; shell state "
        "(variables, aliases) does not. Do not use it to read, search or edit files: read_file, grep, glob and edit "
        "exist for that and are cheaper to review. Quote paths with spaces. Prefer absolute paths over cd. Chain "
        "dependent steps with && and put independent ones in separate parallel calls. Do not poll with sleep; a slow "
        "command moves to the background and you are told when it ends. Never skip git hooks or force-push unless asked, "
        "and run `git status` before anything that discards changes."
    )

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

        from safety.sandbox import wrap_command

        process = await asyncio.create_subprocess_exec(
            *wrap_command(params.command, shell_cmd, self.config),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            stdin=asyncio.subprocess.DEVNULL,
            cwd=cwd,
            env=env,
            start_new_session=True,
        )

        # output is copied to a log as it arrives, so a slow command can be handed
        # over to the background mid-run without losing what it already printed
        job_id = len(JOBS) + 1
        log = Path(tempfile.gettempdir()) / f"melon-job-{os.getpid()}-{job_id}.log"
        log.write_bytes(b"")
        bufs: dict[str, bytearray] = {"out": bytearray(), "err": bytearray()}

        async def pump(stream, key):
            with open(log, "ab") as f:
                while chunk := await stream.read(65536):
                    bufs[key] += chunk
                    f.write(chunk)
                    f.flush()

        pumps = asyncio.gather(pump(process.stdout, "out"), pump(process.stderr, "err"))
        finished = asyncio.ensure_future(asyncio.gather(pumps, process.wait()))

        # background=true detaches at once; otherwise a command that is still running
        # after AUTO_BACKGROUND_AFTER seconds (a build, a dev server) is detached too
        wait_for = 0 if params.background else min(params.timeout, AUTO_BACKGROUND_AFTER)
        try:
            await asyncio.wait_for(asyncio.shield(finished), timeout=wait_for or 0.01)
        except asyncio.TimeoutError:
            if params.background or params.timeout > AUTO_BACKGROUND_AFTER:
                JOBS[job_id] = BackgroundJob(job_id, params.command, log, process)
                return ToolResult.success_result(
                    f"{'Started' if params.background else 'Still running after '+str(AUTO_BACKGROUND_AFTER)+'s, moved to'} "
                    f"background job {job_id}: {params.command}\n"
                    f"Output so far and onward: {log} (read it with `tail`). Stop it with `kill -- -{process.pid}`. "
                    "You will be told when it finishes; keep working on other things meanwhile.",
                    metadata={"background_job": job_id},
                )
            self._kill(process)
            await process.wait()
            return ToolResult.error_result(f"Command timed out after {params.timeout}s")
        except asyncio.CancelledError:
            # user interrupt: never leave the process group running
            self._kill(process)
            await process.wait()
            raise

        stdout_data, stderr_data = bytes(bufs["out"]), bytes(bufs["err"])
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

    @staticmethod
    def _kill(process) -> None:
        if sys.platform != "win32":
            try:
                os.killpg(os.getpgid(process.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
        else:
            process.kill()

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
