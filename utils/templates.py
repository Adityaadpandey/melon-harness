"""Prompt templates: a markdown file in a prompts folder becomes a /command.

    ~/.pi/agent/prompts/review.md      ->  /review  or  /review concurrency

The body is sent as the user's message after the arguments are filled in:
    $1 $2      one argument            ${1:-default}  first argument or a default
    $@         all arguments           ${@:2}         arguments from the second on
Quote to keep spaces together: /review "API compatibility"."""
from __future__ import annotations

import re
import shlex
from pathlib import Path

PROMPT_DIRS = (Path.home() / ".pi" / "agent" / "prompts", Path.home() / ".mew" / "prompts")


def load_templates(dirs=PROMPT_DIRS) -> dict[str, str]:
    found: dict[str, str] = {}
    for d in dirs:
        if d.is_dir():
            for f in sorted(d.glob("*.md")):
                body = f.read_text(encoding="utf-8")
                m = re.match(r"---\n.*?\n---\n(.*)", body, re.S)
                found[f.stem] = (m.group(1) if m else body).strip()
    return found


def split_args(text: str) -> list[str]:
    try:
        return shlex.split(text)
    except ValueError:  # unbalanced quote: fall back to plain words rather than refusing
        return text.split()


def expand(body: str, args: list[str]) -> str:
    def sub(m: re.Match) -> str:
        which, start, default = m.group(1) or m.group(4), m.group(2), m.group(3)
        if start is not None:  # ${@:N}
            picked = args[int(start) - 1:]
        elif which in ("@", "ARGUMENTS"):
            picked = args
        else:
            picked = args[int(which) - 1:int(which)]
        return " ".join(picked) if picked else (default or "")

    return re.sub(r"\$\{(@|\d+)(?::(\d+)|:-([^}]*))?\}|\$(@|ARGUMENTS|\d+)", sub, body)
