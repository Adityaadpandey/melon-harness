"""Repository rules enforced in code. A prompt saying "never read .env" is a
request; this makes it a boundary, for the main agent, subagents and workers
alike, because every tool call passes through ToolRegistry.invoke."""
from __future__ import annotations

import fnmatch
import re
import shlex
from pathlib import Path
from typing import Any

from safety.workspace import Workspace

PATH_KEYS = {"path", "file_path", "cwd", "directory", "dir", "paths", "source", "destination"}
# templates document which secrets exist without holding any: .env.example stays readable while .env.prod is not
TEMPLATE_SUFFIXES = (".example", ".sample", ".template", ".dist")


def is_forbidden_path(path: str | Path, patterns: list[str]) -> str | None:
    """Matching pattern if any part of the path (file or parent dir name) hits."""
    if not patterns:
        return None
    for part in Path(str(path)).parts:
        low = part.lower()
        if low.endswith(TEMPLATE_SUFFIXES):
            continue
        for pat in patterns:
            if fnmatch.fnmatch(low, pat.lower()):
                return pat
    return None


def _tokens(command: str) -> list[str]:
    try:
        toks = shlex.split(command, posix=True)
    except ValueError:
        toks = command.split()
    # also split on redirections / pipes glued to words ("cat<.env") and on git's
    # rev:path spelling, which reads a file out of history ("git show HEAD:.env")
    out: list[str] = []
    for t in toks:
        out.extend(x for x in re.split(r"[<>|;&=:]", t) if x)
    return out


# tools that only look: they may open skill files outside the workspace, nothing else
READ_TOOLS = {"read_file", "list_dir", "grep", "glob", "outline"}

BUILTIN_TOOLS = {"read_file", "write_file", "edit", "shell", "list_dir", "grep", "search", "outline", "git", "glob", "web_search", "web_fetch", "todos", "memory", "use_skill", "graph", "parallel_tasks"}


def _in_skill_dir(path: str, config) -> bool:
    from agent.skills import skill_roots

    real = Path(path).expanduser()
    real = (real if real.is_absolute() else Path(config.cwd) / real).resolve()
    return any(real == r or real.is_relative_to(r) for r in skill_roots(config))


def violation(tool_name: str, params: dict[str, Any], config) -> str | None:
    paths, cmds = config.forbidden_paths, config.forbidden_commands
    ws = Workspace(config.cwd, config.allowed_dirs)

    for key, value in params.items():
        if key in PATH_KEYS:
            for v in value if isinstance(value, list) else [value]:
                if not isinstance(v, str):
                    continue
                if not ws.contains(v) and not (tool_name in READ_TOOLS and _in_skill_dir(v, config)):
                    return f"path '{v}' is outside the workspace ({ws.root}). Work inside the workspace, or ask the user to add the directory with --add-dir"
                if hit := is_forbidden_path(v, paths):
                    return f"path '{v}' matches protected pattern '{hit}'"

    if tool_name not in BUILTIN_TOOLS:
        # MCP and other external tools: scan every string argument for paths
        for v in params.values():
            if isinstance(v, str) and (why := ws.shell_violation(v)):
                return why

    command = params.get("command")
    if isinstance(command, str):
        if why := ws.shell_violation(command, params.get("cwd") and ws.root / str(params["cwd"])):
            from agent.skills import skill_roots

            # the model reaches for `ls ~/.pi/agent/skills/x` in the shell: say where skill files can be read instead of leaving it to retry
            if any(str(r) in why for r in skill_roots(config)):
                why += ". Skill files are readable with read_file, list_dir or grep, not the shell"
            return why
        for pat in cmds:
            if re.search(pat, command, re.IGNORECASE):
                return f"command matches forbidden pattern '{pat}'"
        for tok in _tokens(command):
            if hit := is_forbidden_path(tok, paths):
                return f"command touches '{tok}' (protected pattern '{hit}')"
    return None
