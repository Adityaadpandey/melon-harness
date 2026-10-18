"""What a review request points at: the working tree, the last N commits, one commit, the branch, or the PR checked out here.

    "review my changes"          -> the working tree against HEAD
    "review my last 3 commits"   -> HEAD~3..HEAD
    "review commit 9fceb02"      -> 9fceb02^..9fceb02
    "review this branch / PR"    -> the merge base with main, through the working tree
    "review PR #42"              -> as the branch; when #42 is not checked out its head is fetched into a throwaway worktree

Anything it cannot resolve returns None and the normal single-agent review answers instead."""
from __future__ import annotations

import asyncio
import re
import shutil
import tempfile
from dataclasses import dataclass, field
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
    notes: list[str] = field(default_factory=list)  # where the scope is not what the user asked for ("gh could not name the PR's base, used origin/main")
    commits: list[str] = field(default_factory=list)  # "abc1234 subject" of each commit in the range, so the reader sees what was covered
    cwd: Path | None = None  # a throwaway worktree holding the PR head, when the PR is not the checked-out branch
    repo: Path | None = None  # the repo that worktree belongs to

    @property
    def includes_worktree(self) -> bool:
        return len(self.rev) == 1

    async def cleanup(self) -> None:
        if self.cwd:
            await _ok(self.repo or self.cwd, "worktree", "remove", "--force", str(self.cwd))
            shutil.rmtree(self.cwd, ignore_errors=True)


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


async def _pr_info(cwd: Path, number: str = "") -> tuple[str, str]:
    """(head branch, base branch) of a PR from gh; the current branch's PR when no number is given. ("", "") when gh cannot say.
    Example: a PR from feat/x into develop gives ("feat/x", "develop"), so the review diffs against develop, not main."""
    try:
        proc = await asyncio.create_subprocess_exec(
            "gh", "pr", "view", *([number] if number else []), "--json", "headRefName,baseRefName", "-q", ".headRefName + \" \" + .baseRefName", cwd=cwd,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL, stdin=asyncio.subprocess.DEVNULL)
        out, _ = await asyncio.wait_for(proc.communicate(), 15)
    except (OSError, asyncio.TimeoutError):
        return "", ""
    head, _, base = out.decode().strip().partition(" ")
    return head, base


async def _remote_ref(cwd: Path, base: str) -> str:
    for ref in (f"origin/{base}", base):
        if await _ok(cwd, "rev-parse", "--verify", "-q", ref):
            return ref
    return ""


async def resolve(cwd: Path, message: str, skip: tuple[str, ...]) -> Target | None:
    """None when the request names something we cannot read from here (a PR that is not checked out, a commit that does not exist)."""
    if m := PR_NUMBER.search(message):
        number = m.group(1) or m.group(2)
        head, base = await _pr_info(cwd, number)
        if not head:
            return None  # gh cannot say what this PR is
        if head != await _ok(cwd, "rev-parse", "--abbrev-ref", "HEAD"):
            return await _remote_pr(cwd, number, skip, base)
        return await _branch(cwd, f"PR #{number}", skip, base)
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
        _, base = await _pr_info(cwd)  # this branch's open PR names its real target
        return await _branch(cwd, "this branch", skip, base, ["gh could not name this branch's PR base, so main/master is assumed"] if not base else [])
    return None


async def _refresh(cwd: Path, ref: str) -> bool:
    """Bring origin/<base> up to date first. A stale local copy puts the merge base far back, and then the review
    reads every commit that landed on the base since (a homepage change that is nobody's PR). Offline: carry on with what is there."""
    if not ref.startswith("origin/"):
        return True
    return await _fetch(cwd, ref.split("/", 1)[1])


async def _fetch(cwd: Path, *refspec: str) -> bool:
    try:
        proc = await asyncio.create_subprocess_exec("git", "fetch", "-q", "origin", *refspec, cwd=cwd,
                                                    stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL, stdin=asyncio.subprocess.DEVNULL)
        await asyncio.wait_for(proc.communicate(), 40)
        return proc.returncode == 0
    except (OSError, asyncio.TimeoutError):
        return False


async def _commit_list(cwd: Path, mb: str, tip: str = "HEAD") -> list[str]:
    return [ln for ln in (await _git(cwd, "log", "--format=%h %s", "--max-count=12", f"{mb}..{tip}")).split("\n") if ln]


async def _branch(cwd: Path, name: str, skip: tuple[str, ...], pr_base: str = "", notes: list[str] | None = None) -> Target | None:
    """`pr_base` is the branch the PR merges into; without a PR it falls back to main/master. Every fallback is written into
    `Target.notes`: a review of the wrong range that does not say so is worse than no review."""
    notes = list(notes or [])
    base = (await _remote_ref(cwd, pr_base) if pr_base else "") or ""
    if pr_base and not base:
        notes.append(f"the PR's base branch {pr_base} is not here, so main/master is assumed")
    base = base or await _base_branch(cwd)
    if not await _refresh(cwd, base):
        notes.append(f"could not fetch {base}; its local copy may be stale and the range may include other people's commits")
    mb = await _ok(cwd, "merge-base", base, "HEAD") if base else ""
    if not mb or mb == await _ok(cwd, "rev-parse", "HEAD"):
        return None  # on the base branch itself: the working tree is the only change there is
    files = await _files(cwd, (mb,), skip)
    commits = await _commit_list(cwd, mb)
    return Target(f"{name} against {base} ({len(commits)} commit(s) since {mb[:8]}, plus any uncommitted work)", (mb,), files, notes, commits) if files else None


async def _remote_pr(cwd: Path, number: str, skip: tuple[str, ...], pr_base: str) -> Target | None:
    """A PR whose branch is not checked out: fetch its head (GitHub keeps it at pull/<n>/head), check it out in a throwaway worktree and
    review that. Example: standing on main, "review PR #42" reads the PR's files, not main's."""
    ref = f"refs/mew/pr-{number}"
    if not await _fetch(cwd, f"+pull/{number}/head:{ref}"):
        return None
    base = (await _remote_ref(cwd, pr_base) if pr_base else "") or await _base_branch(cwd)
    notes = [] if pr_base and base.endswith(pr_base) else [f"the PR's base {pr_base or '(unknown)'} is not here, so {base or 'nothing'} is assumed"]
    if not await _refresh(cwd, base):
        notes.append(f"could not fetch {base}; its local copy may be stale and the range may include other people's commits")
    mb = await _ok(cwd, "merge-base", base, ref) if base else ""
    if not mb:
        return None
    tree = Path(tempfile.mkdtemp(prefix=f"mew-pr-{number}-"))
    if not await _ok(cwd, "worktree", "add", "--detach", str(tree), ref) and not (tree / ".git").exists():
        shutil.rmtree(tree, ignore_errors=True)
        return None
    files = await _files(tree, (mb,), skip)
    commits = await _commit_list(cwd, mb, ref)
    if not files:
        await Target("", (), [], cwd=tree, repo=cwd).cleanup()
        return None
    return Target(f"PR #{number} against {base} ({len(commits)} commit(s) since {mb[:8]})", (mb,), files, notes, commits, tree, cwd)
