import asyncio
import re
from pathlib import Path

from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from utils.paths import resolve_path
from utils.search import list_files, search, similar_paths


class GrepParams(BaseModel):
    pattern: str = Field(..., description="Regex (or literal text with literal=true) to search for")
    path: str = Field(".", description="File or directory to search (default: working directory)")
    glob: str | None = Field(None, description="Only search files matching this glob, e.g. '*.ts' or 'src/**/*.spec.ts'")
    case_insensitive: bool | None = Field(
        None, description="Default is smart case: insensitive unless the pattern has an uppercase letter"
    )
    literal: bool = Field(False, description="Treat pattern as plain text, not a regex")
    context: int = Field(0, ge=0, le=10, description="Lines of context before and after each match")
    limit: int = Field(100, ge=1, le=1000, description="Maximum matches to return (default 100)")
    files_only: bool = Field(False, description="Return only the files that match, with counts (cheap way to locate things)")


class GrepTool(Tool):
    name = "grep"
    description = (
        "Search file contents. Respects .gitignore, finds dotfiles, never reads secrets. "
        "Output: path:line:text (context lines use '-'). Use files_only to locate, then read what matters."
    )
    kind = ToolKind.READ
    schema = GrepParams

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = GrepParams(**invocation.params)
        target = resolve_path(invocation.cwd, p.path)
        if not target.exists():
            return ToolResult.error_result(
                f"Path does not exist: {target}." + similar_paths(Path(invocation.cwd), target, self.config.forbidden_paths)
            )

        smart = p.case_insensitive if p.case_insensitive is not None else not any(c.isupper() for c in p.pattern)
        try:
            regex = re.compile(re.escape(p.pattern) if p.literal else p.pattern, re.IGNORECASE if smart else 0)
        except re.error as e:
            return ToolResult.error_result(f"Invalid regex: {e}. Use literal=true for plain text.")

        root = target if target.is_dir() else target.parent
        files = await asyncio.to_thread(list_files, root, self.config.forbidden_paths) if target.is_dir() else [target]
        lines, total, hit_files = await asyncio.to_thread(
            search, files, root, regex, context=p.context, limit=p.limit, glob=p.glob, files_only=p.files_only
        )
        if not p.files_only and total > p.limit and hit_files > 15:
            # too broad to read line by line: show where matches live instead
            lines, total, hit_files = await asyncio.to_thread(
                search, files, root, regex, context=0, limit=40, glob=p.glob, files_only=True
            )
            lines.append(f"{total} matches across {hit_files} files is too broad to list. Narrow with path/glob or a more specific pattern, then read.")
            return ToolResult.success_result("\n".join(lines), metadata={"matches": total, "files_with_matches": hit_files, "files_searched": len(files)})
        meta = {"matches": total, "files_with_matches": hit_files, "files_searched": len(files)}
        if not lines:
            return ToolResult.success_result(f"No matches for '{p.pattern}' in {len(files)} files", metadata=meta)
        if not p.files_only and total > p.limit:
            lines.append(f"... {total - p.limit} more matches in {hit_files} files not shown; narrow the pattern, path or glob")
        return ToolResult.success_result("\n".join(lines), metadata=meta)
