"""Small, safe repairs for tool calls open models get almost right.

Claude rarely needs these; DeepSeek, GLM, Qwen and Kimi often send `Read` for read_file, `old_str` for
old_string, or a list of edits as a JSON string. Rejecting those costs a whole round trip and often a
second wrong attempt, so we fix what has exactly one sensible reading and leave everything else to the
normal validation error.
"""
from __future__ import annotations

import json
import re
import types
from typing import Any, Union, get_args, get_origin

from pydantic import BaseModel

# names other harnesses use for our tools; only applied when the real name is offered in this run
TOOL_ALIASES = {
    "read": "read_file", "view": "read_file", "cat": "read_file",
    "write": "write_file", "create_file": "write_file",
    "edit_file": "edit", "str_replace": "edit", "str_replace_editor": "edit", "multiedit": "edit", "apply_edit": "edit",
    "bash": "shell", "run": "shell", "run_command": "shell", "execute_command": "shell", "terminal": "shell",
    "ls": "list_dir", "list_files": "list_dir", "list_directory": "list_dir",
    "find": "glob", "find_files": "glob", "search_files": "grep", "ripgrep": "grep", "rg": "grep",
    "todo": "todos", "todowrite": "todos", "todo_write": "todos",
}

# parameter names from Claude Code, opencode and SWE-agent style tools
PARAM_ALIASES = {
    "old_str": "old_string", "oldstring": "old_string", "old_text": "old_string", "search": "old_string",
    "new_str": "new_string", "newstring": "new_string", "new_text": "new_string", "replace": "new_string",
    "file_path": "path", "filepath": "path", "file": "path", "filename": "path", "absolute_path": "path",
    "directory": "path", "dir": "path",
    "cmd": "command", "replaceall": "replace_all", "line_count": "limit", "start_line": "offset",
}


def fix_tool_name(name: str | None, offered: set[str]) -> str | None:
    """`Read` -> read_file, `Bash` -> shell; an unknown name comes back unchanged so the caller can list the real ones."""
    if not name or name in offered:
        return name
    low = name.lower()
    for n in offered:
        if n.lower() == low:
            return n
    alias = TOOL_ALIASES.get(low.replace("-", "_"))
    return alias if alias in offered else name


def _kinds(annotation: Any) -> set[str]:
    """What shapes a field accepts: {'list'}, {'dict'}, {'str'}... with Optional unwrapped."""
    origin = get_origin(annotation)
    if origin in (Union, types.UnionType):
        return set().union(*(_kinds(a) for a in get_args(annotation) if a is not type(None)))
    if origin in (list, tuple, set) or annotation in (list, tuple, set):
        return {"list"}
    if origin is dict or annotation is dict or (isinstance(annotation, type) and issubclass(annotation, BaseModel)):
        return {"dict"}
    return {getattr(annotation, "__name__", str(annotation))}


def fix_arguments(schema: Any, args: dict[str, Any]) -> dict[str, Any]:
    """Rename near-miss keys and undo common encoding slips, only where the schema allows one reading.
    e.g. {"file_path": "a.py", "old_str": "x", "new_str": "y"} -> {"path": "a.py", "old_string": "x", "new_string": "y"}
         {"path": "a.py", "edits": "[{\\"old_string\\": ...}]"} -> edits parsed into a real list."""
    if not (isinstance(schema, type) and issubclass(schema, BaseModel)) or not isinstance(args, dict):
        return args
    fields = schema.model_fields
    out = dict(args)
    for key in list(out):
        if key in fields:
            continue
        snake = re.sub(r"(?<!^)([A-Z])", r"_\1", key).lower()
        target = snake if snake in fields else PARAM_ALIASES.get(key.lower())
        if target in fields and target not in out:
            out[target] = out.pop(key)
    for key, value in list(out.items()):
        field = fields.get(key)
        if field is None:
            continue
        if value is None and not field.is_required():
            del out[key]  # "limit": null means "no limit", which is what leaving it out says
            continue
        kinds = _kinds(field.annotation)
        if isinstance(value, str) and "str" not in kinds and value.strip()[:1] in ("[", "{"):
            try:
                value = out[key] = json.loads(value)
            except ValueError:
                continue
        if isinstance(value, dict) and kinds == {"list"}:
            out[key] = [value]  # one edit sent on its own instead of a list of one
    return out
