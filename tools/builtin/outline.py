from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from utils.outline import outline
from utils.paths import resolve_path


class OutlineParams(BaseModel):
    path: str = Field(..., description="Source file to outline")


class OutlineTool(Tool):
    name = "outline"
    description = (
        "List the functions, classes, types and methods of a file with their line ranges (about 30 tokens per symbol "
        "instead of the whole file). Then read one with read_file(path, symbol='name') or an offset/limit slice. "
        "Use it on any file over ~150 lines before reading."
    )
    kind = ToolKind.READ
    schema = OutlineParams

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = OutlineParams(**invocation.params)
        path = resolve_path(invocation.cwd, p.path)
        if not path.is_file():
            return ToolResult.error_result(f"File not found: {path}")
        src = path.read_text(encoding="utf-8", errors="replace")
        syms = outline(path.name, src)
        total = len(src.splitlines())
        if not syms:
            return ToolResult.success_result(f"{total} lines, no symbols recognised. Read it with offset/limit.")
        rows = [f"{s.start}-{s.end}  {'  ' * (s.indent // 2 if s.indent < 8 else 4)}{s.kind} {s.name}" for s in syms[:200]]
        more = f"\n... {len(syms) - 200} more symbols" if len(syms) > 200 else ""
        return ToolResult.success_result(f"{path.name}: {total} lines\n" + "\n".join(rows) + more)
