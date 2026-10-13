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


def is_forbidden_path(path: str | Path, patterns: list[str]) -> str | None:
    """Matching pattern if any part of the path (file or parent dir name) hits."""
    if not patterns:
        return None
    for part in Path(str(path)).parts:
        low = part.lower()
        for pat in patterns:
            if fnmatch.fnmatch(low, pat.lower()):
                return pat
    return None


def _tokens(command: str) -> list[str]:
    try:
        toks = shlex.split(command, posix=True)
    except ValueError:
        toks = command.split()
    # also split on redirections / pipes glued to words, e.g. "cat<.env"
    out: list[str] = []
    for t in toks:
        out.extend(x for x in re.split(r"[<>|;&=]", t) if x)
    return out


BUILTIN_TOOLS = {"read_file", "write_file", "edit", "shell", "list_dir", "grep", "search", "outline", "git", "glob", "web_search", "web_fetch", "todos", "memory", "use_skill", "graph", "parallel_tasks"}


def violation(tool_name: str, params: dict[str, Any], config) -> str | None:
    paths, cmds = config.forbidden_paths, config.forbidden_commands
    ws = Workspace(config.cwd, config.allowed_dirs)

    for key, value in params.items():
        if key in PATH_KEYS:
            for v in value if isinstance(value, list) else [value]:
                if not isinstance(v, str):
                    continue
                if not ws.contains(v):
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
            return why
        for pat in cmds:
            if re.search(pat, command, re.IGNORECASE):
                return f"command matches forbidden pattern '{pat}'"
        for tok in _tokens(command):
            if hit := is_forbidden_path(tok, paths):
                return f"command touches '{tok}' (protected pattern '{hit}')"
    return None
