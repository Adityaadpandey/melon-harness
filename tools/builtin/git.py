import asyncio
import re
from typing import Literal

from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult

MAX_CHARS = 6000
_REV = re.compile(r"^[\w./~^@{}:-]+$")


class GitParams(BaseModel):
    op: Literal["status", "diff", "log", "show", "blame", "find_change"] = Field(
        ..., description=(
            "status: changed files. diff: stat of working tree (or of `path`, full). log: recent commits touching `path`. "
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
        "Needs no approval. Changing history (add/commit/checkout/reset) is not available here."
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
