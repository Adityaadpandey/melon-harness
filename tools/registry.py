from pathlib import Path
from typing import Any
from config.config import Config
from hooks.hook_system import HookSystem
from safety.guard import violation
from safety.approval import ApprovalContext, ApprovalDecision, ApprovalManager
from tools.base import Tool, ToolInvocation, ToolResult
import logging
from tools.builtin import ReadFileTool, get_all_builtin_tools
from tools.subagents import SubagentTool, get_default_subagent_definitions

logger = logging.getLogger(__name__)


class ToolRegistry:
    tracer = None  # set by Session; tool timing/outcome go to the trace
    epoch = 0
    progress = None  # set by the UI: callable(call_id, info)
    _read_state: dict = {}

    def __init__(self, config: Config):
        self._tools: dict[str, Tool] = {}
        self._mcp_tools: dict[str, Tool] = {}
        self._read_state: dict[str, tuple[int, int]] = {}  # path -> (mtime_ns, size) when last seen by the agent
        self.config = config

    @property
    def connected_mcp_servers(self) -> list[Tool]:
        return self._mcp_tools.values()

    def register(self, tool: Tool) -> None:
        if tool.name in self._tools:
            logger.warning(f"Overwriting existing tool: {tool.name}")

        self._tools[tool.name] = tool
        logger.debug(f"Registered tool: {tool.name}")

    def register_mcp_tool(self, tool: Tool) -> None:
        self._mcp_tools[tool.name] = tool
        logger.debug(f"Registered MCP tool: {tool.name}")

    def unregister(self, name: str) -> bool:
        if name in self._tools:
            del self._tools[name]
            return True

        return False

    def get(self, name: str) -> Tool | None:
        if name in self._tools:
            return self._tools[name]
        elif name in self._mcp_tools:
            return self._mcp_tools[name]

        return None

    def get_tools(self) -> list[Tool]:
        tools: list[Tool] = []

        for tool in self._tools.values():
            tools.append(tool)

        for mcp_tool in self._mcp_tools.values():
            tools.append(mcp_tool)

        if self.config.allowed_tools:
            allowed_set = set(self.config.allowed_tools)
            tools = [t for t in tools if t.name in allowed_set]

        return tools

    def get_schemas(self) -> list[dict[str, Any]]:
        return [tool.to_openai_schema() for tool in self.get_tools()]

    async def invoke(self, name, params, cwd, hook_system, approval_manager=None, call_id=""):
        import time

        started = time.perf_counter()
        result = self._unread_edit_error(name, params, cwd) or await self._invoke(name, params, cwd, hook_system, approval_manager, call_id)
        self._note_file_state(name, params, cwd, result)
        self._cap_output(result)
        if self.tracer:
            self.tracer.emit(
                "tool_call", tool=name, ok=result.success,
                sec=round(time.perf_counter() - started, 3),
                blocked=bool(result.metadata.get("blocked")),
                chars=len(result.output or ""), target=_target_label(name, params),
                error=(result.error or "")[:200] if not result.success else None,
            )
        return result

    def _target(self, params: dict[str, Any], cwd: Path) -> Path | None:
        from utils.paths import resolve_path

        raw = params.get("path")
        return resolve_path(cwd, raw) if isinstance(raw, str) else None

    @staticmethod
    def _stamp(p: Path) -> tuple[int, int] | None:
        try:
            st = p.stat()
            return (st.st_mtime_ns, st.st_size)
        except OSError:
            return None

    def _unread_edit_error(self, name: str, params: dict[str, Any], cwd: Path) -> ToolResult | None:
        """Editing from memory is how wrong edits happen. An existing file must have been
        read (and be unchanged since) before edit/write_file may touch it."""
        if not self.config.require_read_before_edit or name not in {"edit", "write_file"}:
            return None
        path = self._target(params, cwd)
        if path is None or not path.is_file():
            return None  # creating a new file needs no prior read
        seen = self._read_state.get(str(path))
        if seen is None:
            return ToolResult.error_result(
                f"Read {params['path']} with read_file before editing it, so the change is based on its real contents.",
                metadata={"tool_name": name, "needs_read": True},
            )
        if seen != self._stamp(path):
            return ToolResult.error_result(
                f"{params['path']} changed on disk since you read it. Re-read it before editing.",
                metadata={"tool_name": name, "needs_read": True},
            )
        return None

    def _note_file_state(self, name: str, params: dict[str, Any], cwd: Path, result: ToolResult) -> None:
        if name in {"read_file", "edit", "write_file"} and result.success:
            path = self._target(params, cwd)
            if path is not None and (stamp := self._stamp(path)):
                self._read_state[str(path)] = stamp  # after our own write we know the content too

    def _cap_output(self, result: ToolResult) -> None:
        """One tool result must never be able to eat the context: keep the head
        and the tail (errors and summaries usually live at the end)."""
        limit = self.config.tool_output_max_chars
        out = result.output
        if len(out) > limit:
            head, tail = int(limit * 0.6), int(limit * 0.4)
            result.output = (
                out[:head]
                + f"\n... [{len(out) - limit} chars omitted; narrow the command or read a slice] ...\n"
                + out[-tail:]
            )
            result.truncated = True

    async def _invoke(
        self,
        name: str,
        params: dict[str, Any],
        cwd: Path,
        hook_system: HookSystem,
        approval_manager: ApprovalManager | None = None,
        call_id: str = "",
    ) -> ToolResult:
        tool = self.get(name)
        if tool is None:
            result = ToolResult.error_result(
                f"Unknown tool: {name}",
                metadata={"tool_name": name},
            )
            await hook_system.trigger_after_tool(name, params, result)
            return result

        if set(params) == {"raw_arguments"}:
            # the client could not parse the model's arguments as JSON
            result = ToolResult.error_result(
                "Tool arguments were not valid JSON. Re-send the call with a "
                f"well-formed JSON object. Received: {str(params['raw_arguments'])[:200]}",
                metadata={"tool_name": name},
            )
            await hook_system.trigger_after_tool(name, params, result)
            return result

        validation_errors = tool.validate_params(params)
        if validation_errors:
            result = ToolResult.error_result(
                f"Invalid parameters: {'; '.join(validation_errors)}",
                metadata={
                    "tool_name": name,
                    "validation_errors": validation_errors,
                },
            )

            await hook_system.trigger_after_tool(name, params, result)

            return result

        blocked = violation(name, params, self.config)
        if blocked:
            result = ToolResult.error_result(
                f"Blocked by repository rules: {blocked}. Do not retry this; "
                "ask the user to provide what you need.",
                metadata={"tool_name": name, "blocked": True},
            )
            await hook_system.trigger_after_tool(name, params, result)
            return result

        await hook_system.trigger_before_tool(name, params)
        invocation = ToolInvocation(params=params, cwd=cwd, epoch=self.epoch, call_id=call_id, progress=self.progress)
        if approval_manager:
            confirmation = await tool.get_confirmation(invocation)
            if confirmation:
                context = ApprovalContext(
                    tool_name=name,
                    params=params,
                    is_mutating=tool.is_mutating(params),
                    affected_paths=confirmation.affected_paths,
                    command=confirmation.command,
                    is_dangerous=confirmation.is_dangerous,
                    tool_kind=tool.kind.value,
                )

                decision = await approval_manager.check_approval(context)
                if decision == ApprovalDecision.REJECTED:
                    result = ToolResult.error_result(
                        "Operation rejected by safety policy"
                    )
                    await hook_system.trigger_after_tool(name, params, result)
                    return result
                elif decision == ApprovalDecision.NEEDS_CONFIRMATION:
                    answer = await approval_manager.request_confirmation(confirmation)

                    if not answer.approved:
                        msg = "User rejected the operation."
                        if answer.feedback:
                            msg += f" {answer.feedback}"
                        result = ToolResult.error_result(msg, metadata={"tool_name": name, "rejected": True})
                        await hook_system.trigger_after_tool(name, params, result)
                        return result
                    if answer.always:
                        approval_manager.remember(confirmation)

        try:
            result = await tool.execute(invocation)
        except Exception as e:
            logger.exception(f"Tool {name} raised unexpected error")
            result = ToolResult.error_result(
                f"Internal error: {str(e)}",
                metadata={"tool_name": name},
            )

        await hook_system.trigger_after_tool(name, params, result)
        return result


def create_default_registry(config: Config) -> ToolRegistry:
    registry = ToolRegistry(config)

    for tool_class in get_all_builtin_tools():
        registry.register(tool_class(config))

    from tools.builtin.graph import GraphTool
    from tools.builtin.parallel_tasks import ParallelTasksTool
    from utils.codegraph import locate

    if config.enable_parallel_tasks and any((d / ".git").exists() for d in [config.cwd.resolve(), *config.cwd.resolve().parents][:6]):
        registry.register(ParallelTasksTool(config))

    if locate(config.cwd):  # only advertise the tool when there is a graph to query
        registry.register(GraphTool(config))

    for subagent_def in get_default_subagent_definitions(config):
        registry.register(SubagentTool(config, subagent_def))

    return registry


def _target_label(name: str, params: dict) -> str:
    """Short label for what a call looked at, for the token cost breakdown."""
    for key in ("symbol", "path", "pattern", "query", "command", "op", "goal"):
        v = params.get(key)
        if isinstance(v, str) and v:
            base = params.get("path") if key in ("symbol", "pattern") and isinstance(params.get("path"), str) else None
            return (f"{base}:{v}" if base else v)[:80]
    return ""
