"""Run a review sub-agent as a headless Claude Code session (`claude -p`) instead of a mew agent.

It uses the user's own Claude login and quota, loads the repo's CLAUDE.md and skills the way Claude Code always does, and
gets only read-only tools. Example of what one call runs:

    claude -p "<prompt>" --output-format json --no-session-persistence --tools Read,Grep,Glob,Bash
           --allowedTools "Bash(git log:*)" ... --disallowedTools "Read(**/.env*)" ...

Nothing here talks to an undocumented API: it is the public CLI, so an update of Claude Code cannot break a token grab."""
from __future__ import annotations

import asyncio
import json
import os
import shutil
from pathlib import Path

GIT_READS = ("log", "blame", "show", "diff", "status", "ls-files", "grep", "merge-base", "rev-parse", "branch")


def clean_env() -> dict[str, str]:
    """The environment for the child, minus anything that would point Claude Code at another endpoint or key
    (ANTHROPIC_BASE_URL, ANTHROPIC_API_KEY, ANTHROPIC_AUTH_TOKEN). The review then runs on your own Claude login, never through
    a gateway such as Command Code, even if a shell or .env happens to export them."""
    return {k: v for k, v in os.environ.items() if k not in ("ANTHROPIC_BASE_URL", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN")}


def available() -> bool:
    return shutil.which("claude") is not None


def build_command(config, prompt: str, model: str = "") -> list[str]:
    deny = [f"Read(**/{g})" for g in config.forbidden_paths]  # the repo's secret-file rules follow the review into Claude Code
    cmd = ["claude", "-p", prompt, "--output-format", "json", "--no-session-persistence",
           "--tools", "Read,Grep,Glob,Bash",
           "--allowedTools", "Read", "Grep", "Glob", *(f"Bash(git {g}:*)" for g in GIT_READS),
           "--disallowedTools", "Edit", "Write", "NotebookEdit", *deny]
    if model:
        cmd += ["--model", model]
    return cmd


def parse_output(raw: str) -> tuple[str, int]:
    """(answer text, tokens spent) from `--output-format json`; ("", 0) when it is not what we expect."""
    try:
        data = json.loads(raw)
    except ValueError:
        return "", 0
    if isinstance(data, list):  # some versions print the whole event list: the last result entry is the answer
        data = next((d for d in reversed(data) if isinstance(d, dict) and d.get("type") == "result"), {})
    if not isinstance(data, dict) or data.get("is_error"):
        return "", 0
    usage = data.get("usage") or {}
    return str(data.get("result") or ""), int(usage.get("input_tokens", 0)) + int(usage.get("output_tokens", 0))


async def ask_claude(config, prompt: str, timeout: int = 240) -> tuple[str, int]:
    """One read-only Claude Code run in the repo. A missing CLI, a timeout or a bad reply is an empty answer, never an error."""
    try:
        proc = await asyncio.create_subprocess_exec(
            *build_command(config, prompt, config.review_claude_model), cwd=Path(config.cwd), env=clean_env(),
            stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
        )
        out, _ = await asyncio.wait_for(proc.communicate(), timeout)
    except (OSError, asyncio.TimeoutError):
        return "", 0
    return parse_output(out.decode("utf-8", "replace"))
