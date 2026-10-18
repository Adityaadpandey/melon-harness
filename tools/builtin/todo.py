import uuid
from config.config import Config
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from pydantic import BaseModel, Field


class TodoItem(BaseModel):
    id: str | None = None  # optional: models often name their steps ("inspect") and later complete them by that name
    content: str
    done: bool = False


class TodosParams(BaseModel):
    action: str = Field(..., description="Action: 'set' (replace the whole list in one call), 'add', 'complete', 'list', 'clear'")
    id: str | None = Field(None, description="Todo ID (for complete)")
    content: str | None = Field(None, description="Todo content (for add)")
    items: list[TodoItem] | None = Field(None, description="The full list, for 'set': every step with done true/false. One call replaces many add/complete calls")


class TodosTool(Tool):
    name = "todos"
    description = (
        "Live checklist for work with 3+ steps or several requests. Skip it for a single trivial step. "
        "Prefer `set` with the whole list. Mark a step done the moment it is finished "
        "(never batch completions), and only mark it done when it really passed: if tests fail or the work is partial, keep it open."
    )
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

    def _find(self, ref: str) -> str | None:
        """An id, or failing that the step's text: complete "inspect" works when the step reads "Inspect the nav links"."""
        if ref in self._todos:
            return ref
        hits = [k for k, t in self._todos.items() if ref.lower() in t["content"].lower()]
        return hits[0] if len(hits) == 1 else None

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        params = TodosParams(**invocation.params)

        if params.action.lower() == "set":
            if not params.items:
                return ToolResult.error_result("`items` required for 'set' action")
            self._todos = {(it.id or str(i + 1)): {"content": it.content, "done": it.done} for i, it in enumerate(params.items)}
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
            key = self._find(params.id)
            if key is None:
                return ToolResult.error_result(f"Todo not found: {params.id}. Current ids: {', '.join(self._todos) or 'none'}")

            self._todos[key]["done"] = True  # kept, so the checklist shows what is finished
            return ToolResult.success_result(f"Completed todo [{key}]: {self._todos[key]['content']}")
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
