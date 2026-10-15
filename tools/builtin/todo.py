import uuid
from config.config import Config
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from pydantic import BaseModel, Field


class TodoItem(BaseModel):
    content: str
    done: bool = False


class TodosParams(BaseModel):
    action: str = Field(..., description="Action: 'set' (replace the whole list in one call), 'add', 'complete', 'list', 'clear'")
    id: str | None = Field(None, description="Todo ID (for complete)")
    content: str | None = Field(None, description="Todo content (for add)")
    items: list[TodoItem] | None = Field(None, description="The full list, for 'set': every step with done true/false. One call replaces many add/complete calls")


class TodosTool(Tool):
    name = "todos"
    description = "Manage a task list for the current session. Use this to track progress on multi-step tasks."
    kind = ToolKind.MEMORY
    schema = TodosParams

    def __init__(self, config: Config) -> None:
        super().__init__(config)
        self._todos: dict[str, dict] = {}  # id -> {content, done}

    def snapshot(self) -> list[tuple[str, bool]]:
        """(content, done) in creation order, for the live checklist in the UI."""
        return [(t["content"], t["done"]) for t in self._todos.values()]

    def render(self) -> str:
        """The list as plain text, so it can be put back into the conversation after old results are cleared."""
        return "\n".join(f"[{'x' if t['done'] else ' '}] {t['content']}" for t in self._todos.values())

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        params = TodosParams(**invocation.params)

        if params.action.lower() == "set":
            if not params.items:
                return ToolResult.error_result("`items` required for 'set' action")
            self._todos = {str(i + 1): {"content": it.content, "done": it.done} for i, it in enumerate(params.items)}
            done = sum(it.done for it in params.items)
            return ToolResult.success_result(f"Todo list set: {len(params.items)} steps, {done} done")
        elif params.action.lower() == "add":
            if not params.content:
                return ToolResult.error_result("`content` required for 'add' action")
            todo_id = str(uuid.uuid4())[:8]
            self._todos[todo_id] = {"content": params.content, "done": False}
            return ToolResult.success_result(
                f"Added todo [{todo_id}]: {params.content}"
            )
        elif params.action.lower() == "complete":
            if not params.id:
                return ToolResult.error_result("`id` required for 'complete' action")
            if params.id not in self._todos:
                return ToolResult.error_result(f"Todo not found: {params.id}")

            self._todos[params.id]["done"] = True  # kept, so the checklist shows what is finished
            return ToolResult.success_result(f"Completed todo [{params.id}]: {self._todos[params.id]['content']}")
        elif params.action == "list":
            if not self._todos:
                return ToolResult.success_result("No todos")
            lines = ["Todos:"]

            for todo_id, t in self._todos.items():
                lines.append(f"  [{todo_id}] {'[x]' if t['done'] else '[ ]'} {t['content']}")
            return ToolResult.success_result("\n".join(lines))
        elif params.action == "clear":
            count = len(self._todos)
            self._todos.clear()
            return ToolResult.success_result(f"Cleared {count} todos")
        else:
            return ToolResult.error_result(f"Unknown action: {params.action}")
