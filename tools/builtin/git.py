import asyncio
import re
from typing import Literal

from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult

MAX_CHARS = 6000
PATCH_TOTAL, PATCH_PER_FILE = 24_000, 3_500  # a review reads the whole change in one call, big files are cut so none crowd out the rest
_REV = re.compile(r"^[\w./~^@{}:-]+$")


class GitParams(BaseModel):
    op: Literal["scope", "patch", "status", "diff", "log", "show", "blame", "find_change"] = Field(
        ..., description=(
            "scope: one-call review snapshot (branch, status, base range, stat of working-tree and branch changes). patch: the actual changes, every changed file in ONE call (uncommitted vs HEAD, or vs `rev`); use it instead of one diff call per file. status: changed files. diff: stat of working tree (or of `path`, full). log: recent commits touching `path`. "
            "show: one commit's files (and its diff of `path`). blame: who last changed lines `lines` of `path`. "
            "find_change: commits that added/removed the text `query` (git log -S), the fastest way to find when a behaviour changed."
        ),
    )
    path: str | None = Field(None, description="File to focus on (relative to the working directory)")
    rev: str | None = Field(None, description="Commit, branch or range, e.g. HEAD~3 or main..HEAD")
    lines: str | None = Field(None, description="blame only: 'start,end' line range, e.g. '40,80'")
    query: str | None = Field(None, description="find_change only: text to look for")
    limit: int = Field(8, ge=1, le=30, description="Max commits for log/find_change")


class GitTool(Tool):
    name = "git"
    description = (
        "Read-only git history for finding what changed and why: status, diff, log, show, blame, find_change. "
        "Needs no approval. To add, commit, switch branches, push or pull, run git in the shell tool (`git commit -m ...`)."
    )
    kind = ToolKind.READ
    schema = GitParams

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = GitParams(**invocation.params)
        for label, v in (("rev", p.rev), ("path", p.path), ("query", p.query)):
            if v and v.startswith("-"):
                return ToolResult.error_result(f"{label} may not start with '-'")
        if p.rev and not _REV.match(p.rev):
            return ToolResult.error_result(f"Unusual revision '{p.rev}'")
        tail = ["--", p.path] if p.path else []
        pretty = "--pretty=format:%h %ad %an: %s"
        if p.op == "scope":
            return await self._scope(invocation.cwd, p.rev)
        if p.op == "patch":
            return await self._patch(invocation.cwd, p.rev, p.path)
        if p.op == "status":
            argv = ["status", "--short", "--branch"]
        elif p.op == "diff":
            argv = ["diff", *( [p.rev] if p.rev else [] ), *([] if p.path else ["--stat"]), *tail]
        elif p.op == "log":
            argv = ["log", f"-n{p.limit}", "--date=short", pretty, *([p.rev] if p.rev else []), *tail]
        elif p.op == "show":
            if not p.rev:
                return ToolResult.error_result("show needs rev")
            argv = ["show", "--date=short", pretty, *([] if p.path else ["--stat"]), p.rev, *tail]
        elif p.op == "blame":
            if not (p.path and p.lines and re.fullmatch(r"\d+,\d+", p.lines)):
                return ToolResult.error_result("blame needs path and lines like '40,80'")
            argv = ["blame", "-s", "-w", "-L", p.lines, *([p.rev] if p.rev else []), "--", p.path]
        else:
            if not p.query:
                return ToolResult.error_result("find_change needs query")
            argv = ["log", f"-n{p.limit}", "--date=short", pretty, "--stat", "-S", p.query, *tail]
        proc = await asyncio.create_subprocess_exec(
            "git", "--no-pager", *argv, cwd=invocation.cwd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE
        )
        try:
            out, err = await asyncio.wait_for(proc.communicate(), 30)
        except asyncio.TimeoutError:
            proc.kill()
            return ToolResult.error_result("git timed out")
        if proc.returncode:
            return ToolResult.error_result(err.decode("utf-8", "replace").strip()[:500] or "git failed")
        text = out.decode("utf-8", "replace").strip() or "(no output)"
        if len(text) > MAX_CHARS:
            text = text[:MAX_CHARS] + f"\n... [clipped at {MAX_CHARS} chars; narrow with path/rev/limit]"
        return ToolResult.success_result(text)

    async def _scope(self, cwd, base: str | None) -> ToolResult:
        """Everything a review needs to fix its scope once: where we are, what is uncommitted,
        and which commits and files the branch adds over its base."""
        async def git(*argv: str) -> str:
            proc = await asyncio.create_subprocess_exec(
                "git", "--no-pager", *argv, cwd=cwd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL
            )
            out, _ = await asyncio.wait_for(proc.communicate(), 30)
            return out.decode("utf-8", "replace").strip() if proc.returncode == 0 else ""

        if not await git("rev-parse", "--git-dir"):
            return ToolResult.error_result("not a git repository")
        if not base:  # first of the usual trunk names that exists here
            for cand in ("origin/main", "origin/master", "main", "master"):
                if await git("rev-parse", "--verify", "-q", cand):
                    base = cand
                    break
        head = await git("rev-parse", "--abbrev-ref", "HEAD")
        parts = [f"branch: {head}", f"status:\n{await git('status', '--short') or '(clean)'}",
                 f"uncommitted (staged+unstaged) vs HEAD:\n{await git('diff', '--stat', 'HEAD') or '(none)'}"]
        if base and head != base.split("/")[-1]:
            commits = await git("log", "--oneline", f"{base}..HEAD", "-n", "15")
            parts.append(f"branch commits not in {base}:\n{commits or '(none)'}")
            if commits:
                parts.append(f"branch files vs {base}:\n{await git('diff', '--stat', f'{base}...HEAD')}")
        else:
            parts.append("base: none found, or already on it; only uncommitted changes are in scope")
        return ToolResult.success_result("\n\n".join(parts)[:MAX_CHARS])


    async def _patch(self, cwd, rev: str | None, path: str | None) -> ToolResult:
        """The whole change as one readable patch. Example: 6 changed files cost one call, not six."""
        argv = ["git", "--no-pager", "diff", "--no-color", "-U2", rev or "HEAD", *(["--", path] if path else [])]
        proc = await asyncio.create_subprocess_exec(*argv, cwd=cwd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        out, err = await asyncio.wait_for(proc.communicate(), 30)
        if proc.returncode:
            return ToolResult.error_result(err.decode("utf-8", "replace").strip()[:500] or "git failed")
        files = re.split(r"(?m)^(?=diff --git )", out.decode("utf-8", "replace"))
        files = [f for f in files if f.strip()]
        if not files:
            return ToolResult.success_result("(no changes; new untracked files are not in a diff, read them directly)")
        shown, used, cut, skipped = [], 0, [], []
        for f in files:
            name = f.splitlines()[0].rsplit(" b/", 1)[-1]
            if used >= PATCH_TOTAL:
                skipped.append(name)
                continue
            if len(f) > PATCH_PER_FILE:
                f = f[:PATCH_PER_FILE] + "\n... [file cut here]\n"
                cut.append(name)
            shown.append(f)
            used += len(f)
        note = ""
        if cut:
            note += f"\ncut (read the rest with path=<file>): {', '.join(cut)}"
        if skipped:
            note += f"\nnot shown, over the size limit: {', '.join(skipped)}"
        return ToolResult.success_result("".join(shown).rstrip() + note)
