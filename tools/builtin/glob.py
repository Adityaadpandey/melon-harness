import asyncio

from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from utils.paths import resolve_path
from utils.search import glob_match, list_files

LIMIT = 500


class GlobParams(BaseModel):
    pattern: str = Field(..., description="Glob like '*.py' (any depth by name) or 'src/**/*.ts' (path pattern)")
    path: str = Field(".", description="Directory to search in (default: working directory)")


class GlobTool(Tool):
    name = "glob"
    description = "Find files by glob pattern. Respects .gitignore and finds dotfiles; never lists secrets. Paths are relative to the search directory."
    kind = ToolKind.READ
    schema = GlobParams

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = GlobParams(**invocation.params)
        root = resolve_path(invocation.cwd, p.path)
        if not root.is_dir():
            return ToolResult.error_result(f"Directory does not exist: {root}")
        files = await asyncio.to_thread(list_files, root, self.config.forbidden_paths)
        hits = [f.relative_to(root).as_posix() for f in files if glob_match(f.relative_to(root).as_posix(), p.pattern)]
        if not hits:
            return ToolResult.success_result(f"No files match '{p.pattern}'", metadata={"matches": 0})
        text = "\n".join(hits[:LIMIT])
        if len(hits) > LIMIT:
            text += f"\n... {len(hits) - LIMIT} more not shown; use a narrower pattern or path"
        return ToolResult.success_result(text, metadata={"matches": len(hits)})
