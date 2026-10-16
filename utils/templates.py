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


def prompt_dirs(cwd: Path | None = None) -> tuple[Path, ...]:
    """Pi's and Mew's prompt folders, then Claude Code's command folders, then the repo's own (later ones win)."""
    home = Path.home()
    dirs = [*PROMPT_DIRS, home / ".claude" / "commands", home / ".ai-agent" / "commands"]
    if cwd:
        dirs += [Path(cwd) / ".claude" / "commands", Path(cwd) / ".ai-agent" / "commands"]
    return tuple(dirs)


def load_commands(dirs=PROMPT_DIRS) -> dict[str, tuple[str, str]]:
    """name -> (body, description). `review/security.md` is named `review:security`, as in Claude Code.
    The description is the front-matter line, or the first line of the body."""
    found: dict[str, tuple[str, str]] = {}
    for d in dirs:
        if not d.is_dir():
            continue
        for f in sorted(d.rglob("*.md")):
            try:
                text = f.read_text(encoding="utf-8")
            except OSError:
                continue
            m = re.match(r"---\n(.*?)\n---\n(.*)", text, re.S)
            body = (m.group(2) if m else text).strip()
            meta = dict(ln.split(":", 1) for ln in (m.group(1).splitlines() if m else []) if ":" in ln and not ln[:1].isspace())
            desc = meta.get("description", "").strip().strip("'\"") or next((ln.strip(" #") for ln in body.splitlines() if ln.strip()), "")
            found[":".join(f.relative_to(d).with_suffix("").parts)] = (body, desc[:100])
    return found


def load_templates(dirs=PROMPT_DIRS) -> dict[str, str]:
    return {name: body for name, (body, _) in load_commands(dirs).items()}


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
