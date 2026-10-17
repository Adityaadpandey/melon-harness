"""What a review request points at: the working tree, the last N commits, one commit, the branch, or the PR checked out here.

    "review my changes"          -> the working tree against HEAD
    "review my last 3 commits"   -> HEAD~3..HEAD
    "review commit 9fceb02"      -> 9fceb02^..9fceb02
    "review this branch / PR"    -> the merge base with main, through the working tree
    "review PR #42"              -> as the branch, but only when #42 is the branch checked out (the reviewers read the tree)

Anything it cannot resolve returns None and the normal single-agent review answers instead."""
from __future__ import annotations

import asyncio
import re
from dataclasses import dataclass
from pathlib import Path

from agent.review import _git

NUMBER_WORDS = {"a": 1, "one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7, "eight": 8, "nine": 9, "ten": 10}
SHA = re.compile(r"\b[0-9a-f]{7,40}\b", re.I)  # git decides whether a hex-looking word ("defaced") is a commit
LAST_N = re.compile(r"\b(?:last|latest|recent|past)\s+(\d+|" + "|".join(NUMBER_WORDS) + r")?\s*commits?\b|\b(\d+)\s+commits?\b|\b(?:last|latest)\s+commit\b", re.I)
PR_NUMBER = re.compile(r"(?:\bpr\b|pull request|merge request)\s*#?(\d+)|(?<![\w/])#(\d+)\b", re.I)
EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbb2f2ac"
BRANCH = re.compile(r"\b(branch|pull request|pr|changes we made|whole change|everything we changed)\b", re.I)


@dataclass
class Target:
    label: str  # "the last 3 commits"
    rev: tuple[str, ...]  # arguments for `git diff`: ("HEAD",) is the working tree, ("a", "b") is a committed range
    files: list[str]

    @property
    def includes_worktree(self) -> bool:
        return len(self.rev) == 1


async def _ok(cwd: Path, *args: str) -> str:
    return (await _git(cwd, *args)).strip()


async def _base_branch(cwd: Path) -> str:
    for b in ("origin/main", "origin/master", "main", "master"):
        if await _ok(cwd, "rev-parse", "--verify", "-q", b):
            return b
    return ""


async def _files(cwd: Path, rev: tuple[str, ...], skip: tuple[str, ...]) -> list[str]:
    names = (await _git(cwd, "diff", "--name-only", *rev)).split("\n")
    if len(rev) == 1:  # a working-tree diff leaves out brand-new files git does not know yet
        names += (await _git(cwd, "ls-files", "--others", "--exclude-standard")).split("\n")
    return [n for n in dict.fromkeys(names) if n and not n.endswith(skip)]


async def _pr_matches_branch(cwd: Path, number: str) -> bool:
    try:
        proc = await asyncio.create_subprocess_exec(
            "gh", "pr", "view", number, "--json", "headRefName", "-q", ".headRefName", cwd=cwd,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL, stdin=asyncio.subprocess.DEVNULL)
        out, _ = await asyncio.wait_for(proc.communicate(), 15)
    except (OSError, asyncio.TimeoutError):
        return False
    return out.decode().strip() != "" and out.decode().strip() == await _ok(cwd, "rev-parse", "--abbrev-ref", "HEAD")


async def resolve(cwd: Path, message: str, skip: tuple[str, ...]) -> Target | None:
    """None when the request names something we cannot read from here (a PR that is not checked out, a commit that does not exist)."""
    if m := PR_NUMBER.search(message):
        if not await _pr_matches_branch(cwd, m.group(1) or m.group(2)):
            return None
        return await _branch(cwd, f"PR #{m.group(1) or m.group(2)}", skip)
    for m in SHA.finditer(message):
        sha = m.group(0)
        if not await _ok(cwd, "rev-parse", "--verify", "-q", f"{sha}^{{commit}}"):
            continue
        rev = (f"{sha}^", sha) if await _ok(cwd, "rev-parse", "--verify", "-q", f"{sha}^") else (EMPTY_TREE, sha)
        files = await _files(cwd, rev, skip)
        return Target(f"commit {sha[:10]}", rev, files) if files else None
    if m := LAST_N.search(message):
        raw = (m.group(1) or m.group(2) or "1").lower()
        n = int(raw) if raw.isdigit() else NUMBER_WORDS.get(raw, 1)
        n = max(1, min(n, 20))
        if not await _ok(cwd, "rev-parse", "--verify", "-q", f"HEAD~{n}"):
            return None
        rev = (f"HEAD~{n}", "HEAD")
        files = await _files(cwd, rev, skip)
        return Target(f"the last {n} commit(s)" if n > 1 else "the last commit", rev, files) if files else None
    if BRANCH.search(message):
        return await _branch(cwd, "this branch", skip)
    return None


async def _branch(cwd: Path, name: str, skip: tuple[str, ...]) -> Target | None:
    base = await _base_branch(cwd)
    mb = await _ok(cwd, "merge-base", base, "HEAD") if base else ""
    if not mb or mb == await _ok(cwd, "rev-parse", "HEAD"):
        return None  # on the base branch itself: the working tree is the only change there is
    files = await _files(cwd, (mb,), skip)
    return Target(f"{name} against {base} (its commits and any uncommitted work)", (mb,), files) if files else None
