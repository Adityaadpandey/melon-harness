"""Try to make a confirmed defect actually happen before the reader is told about it.

A verifier that only reads code can be fooled by a plausible chain; a failing script cannot. The repro agent works in a throwaway copy
of the change (never the user's checkout), with the OS sandbox on, and answers one of three ways:

    REPRODUCED      it ran something that shows the defect (the command and the telling output line come back)
    NOT_REPRODUCED  it ran the real code path and the defect did not happen: the finding is capped at MEDIUM
    CANNOT_RUN      no way to run it here (no dependencies, needs a database): the finding stays as the verifier left it"""
from __future__ import annotations

import shutil
import tempfile
from pathlib import Path

from agent.review import _git

REPRO_PROMPT = """A reviewer claims this defect exists in the code under your current directory (a throwaway copy of the change):

{claim}

The verifier's evidence: {evidence}

Prove it by running code, or show that it does not happen. Write the smallest test or script that pushes the exact input from the claim through
the real function (or the real component, route or query) and prints what comes out. Put it in a NEW file named .melon-repro.<ext>; never edit an
existing file. Run it with the project's own runner (look at package.json / pyproject.toml / the neighbouring tests for the command; node_modules
is already linked in). No network, no installs, no database. If the defect needs something you cannot run here, say CANNOT_RUN and why.

Reply with EXACTLY one first line:
REPRODUCED | <the command you ran> | <the one output line that shows the defect>
or
NOT_REPRODUCED | <the command you ran> | <the output line that shows the code does the right thing>
or
CANNOT_RUN | <why>"""

REPRO_TOOLS = ["read_file", "grep", "glob", "list_dir", "search", "outline", "git", "shell", "write_file"]
MAX_REPROS = 4


async def make_scratch(cwd: Path, files: list[str], rev_tip: str = "HEAD", from_worktree: bool = True) -> Path | None:
    """A detached worktree of `rev_tip` holding the changed files as they are now, with node_modules linked in from the real checkout.
    Example: a change under apps/web gets apps/web/node_modules linked so `pnpm vitest` resolves its imports. None when git refuses."""
    tree = Path(tempfile.mkdtemp(prefix="melon-repro-"))
    await _git(cwd, "worktree", "add", "--detach", str(tree), rev_tip)
    if not (tree / ".git").exists():
        shutil.rmtree(tree, ignore_errors=True)
        return None
    if from_worktree:
        for f in files:
            src, dst = cwd / f, tree / f
            if src.is_file():
                dst.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(src, dst)
            elif dst.exists():
                dst.unlink()
    link_dependencies(cwd, tree)
    return tree


def link_dependencies(source: Path, tree: Path) -> None:
    """Symlink every node_modules (root and one or two folders down: apps/web/node_modules) into the copy."""
    for pattern in ("node_modules", "*/node_modules", "*/*/node_modules"):
        for nm in source.glob(pattern):
            rel = nm.relative_to(source)
            if "node_modules" in rel.parts[:-1] or (tree / rel).exists() or not nm.is_dir():
                continue
            (tree / rel).parent.mkdir(parents=True, exist_ok=True)
            (tree / rel).symlink_to(nm.resolve(), target_is_directory=True)


async def drop_scratch(cwd: Path, tree: Path) -> None:
    await _git(cwd, "worktree", "remove", "--force", str(tree))
    shutil.rmtree(tree, ignore_errors=True)


def parse_repro(text: str) -> tuple[str, str]:
    """(REPRODUCED | NOT_REPRODUCED | CANNOT_RUN, the rest of the line). Anything else reads as CANNOT_RUN: a reply nobody can parse proves nothing."""
    for line in (text or "").splitlines():
        head, _, rest = line.strip().lstrip("-*• ").partition("|")
        word = head.strip().upper()
        if word in ("REPRODUCED", "NOT_REPRODUCED", "CANNOT_RUN"):
            return word, rest.strip()[:300]
    return "CANNOT_RUN", ""
