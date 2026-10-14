from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult


class RecallParams(BaseModel):
    id: str = Field(..., description="Result id from a note like [Old tool result cleared (r7 ...)] or [... full output: r7]")
    offset: int = Field(1, ge=1, description="First line to return (1-based)")
    limit: int = Field(200, ge=1, le=400, description="Max lines to return")


class RecallResultTool(Tool):
    name = "recall_result"
    description = (
        "Get back the exact text of an earlier tool result that was cleared from context or cut short "
        "(its note shows an id like r7). Returns a line range; use offset/limit to page through a long one."
    )
    kind = ToolKind.READ
    schema = RecallParams

    def __init__(self, config, store=None):
        super().__init__(config)
        self.store = store

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = RecallParams(**invocation.params)
        text = self.store.get(p.id) if self.store else None
        if text is None:
            return ToolResult.error_result(f"No archived result '{p.id}'. Re-run the original tool instead.")
        lines = text.splitlines()
        part = lines[p.offset - 1 : p.offset - 1 + p.limit]
        end = p.offset - 1 + len(part)
        tail = f"\n[lines {p.offset}-{end} of {len(lines)}; pass offset={end + 1} for more]" if end < len(lines) else ""
        return ToolResult.success_result("\n".join(part) + tail)
