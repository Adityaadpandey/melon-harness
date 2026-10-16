from pydantic import BaseModel, Field

from agent import notes
from config.config import Config
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult


class NotesParams(BaseModel):
    action: str = Field(..., description="'read', 'write' (replace everything) or 'clear'")
    text: str | None = Field(None, description="For 'write': the whole notebook, a few short lines (goal, decided, done, next). Max ~3000 chars")


class NotesTool(Tool):
    name = "notes"
    description = (
        "A durable notebook for a long task, kept across context compaction, /resume and new sessions. Write it after a real decision or a "
        "milestone (goal, decided, done, next), not after every step. Clear it when the task is finished. For the live checklist use todos."
    )
    kind = ToolKind.MEMORY
    schema = NotesParams

    def __init__(self, config: Config) -> None:
        super().__init__(config)

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = NotesParams(**invocation.params)
        action = p.action.lower()
        if action == "read":
            return ToolResult.success_result(notes.read(self.config.cwd) or "(no notes yet)")
        if action == "clear":
            notes.write(self.config.cwd, "")
            return ToolResult.success_result("Notes cleared")
        if action == "write":
            if not p.text or not p.text.strip():
                return ToolResult.error_result("`text` is required for 'write'; use 'clear' to empty the notebook")
            notes.write(self.config.cwd, p.text)
            return ToolResult.success_result(f"Notes saved ({min(len(p.text.strip()), notes.MAX_CHARS)} chars)")
        return ToolResult.error_result("action must be read, write or clear")
