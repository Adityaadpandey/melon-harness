import os
import tempfile
from pathlib import Path
from typing import Any
from config.config import Config
from hooks.hook_system import HookSystem
from safety.guard import violation
from safety.approval import ApprovalContext, ApprovalDecision, ApprovalManager
from tools.base import Tool, ToolInvocation, ToolResult
import json
import logging
from tools.builtin import ReadFileTool, get_all_builtin_tools
from tools.builtin.recall import RecallResultTool
from utils.archive import ResultStore
from tools.subagents import SubagentTool, get_default_subagent_definitions

logger = logging.getLogger(__name__)

PI_MAX_LINES = 2000
PI_MAX_BYTES = 50 * 1024
SHELL_MAX_BYTES = 12 * 1024  # a 28k-char test log costs ~7k tokens on every later call; the tail has the summary, the rest is in the temp file


# read-only lookups whose answer only changes when something is written; read_file has its own finer dedup
REUSABLE = {"grep", "search", "glob", "list_dir", "outline", "git", "graph"}


class ToolRegistry:
    tracer = None  # set by Session; tool timing/outcome go to the trace
    epoch = 0
    gate = None  # set by the agent during a fix run: callable(tool name) -> refusal text or None
    progress = None  # set by the UI: callable(call_id, info)
    _read_state: dict = {}

    def __init__(self, config: Config):
        self._tools: dict[str, Tool] = {}
        self._mcp_tools: dict[str, Tool] = {}
        self._instructions_seen: set = set()  # folders whose AGENTS.md / CLAUDE.md were already attached to a result
        self._read_state: dict[str, tuple[int, int]] = {}  # path -> (mtime_ns, size) when last seen by the agent
        self.config = config
        self._answered: dict[tuple, tuple[str, int]] = {}  # (tool, args, workspace) -> (call id that got the answer, write generation)
        self._writes = 0  # bumps after every call that can change the workspace
        self.is_live = lambda call_id: True  # Session: is that call's result still in the model's context?
        self.archive = ResultStore()  # Session points it at a folder; full copies of cut or cleared results live here

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
        key = (name, json.dumps(params, sort_keys=True, default=str), str(cwd))
        result = self._gated(name) or self._unread_edit_error(name, params, cwd) or self._repeat(name, key)
        if result is None:
            result = await self._invoke(name, params, cwd, hook_system, approval_manager, call_id)
            tool = self.get(name)
            if name == "advisor":
                tool.last_call_at = self._writes  # the edits so far were put to the advisor (an advisor that is down is not nagged about)
            if tool and tool.is_mutating(params):
                self._writes += 1
            elif name in REUSABLE and result.success and call_id:
                self._answered[key] = (call_id, self._writes)
        self._note_file_state(name, params, cwd, result)
        if name in {"read_file", "edit", "write_file"} and result.success and not self.config.pi_mode and (path := self._target(params, cwd)):
            from agent.instructions import for_path

            if extra := for_path(self.config.cwd, path, self._instructions_seen):
                result.output = f"{result.output}\n\n{extra}"
        self._cap_output(result, name)
        if self.tracer:
            self.tracer.emit(
                "tool_call", tool=name, ok=result.success,
                sec=round(time.perf_counter() - started, 3),
                blocked=bool(result.metadata.get("blocked")),
                chars=len(result.output or ""), target=_target_label(name, params),
                error=(result.error or "")[:200] if not result.success else None,
            )
        return result

    def _repeat(self, name: str, key: tuple) -> ToolResult | None:
        """The same lookup again, with nothing written since and the first answer still in
        context: say so in one line instead of sending the same text a second time.
        ponytail: edits made outside this session are not seen; add a git-state stamp if that bites."""
        hit = self._answered.get(key) if name in REUSABLE else None
        if not hit or hit[1] != self._writes or not self.is_live(hit[0]):
            return None
        return ToolResult.success_result(
            f"[unchanged: this exact {name} call was already answered above (call {hit[0]}) and nothing was written since. "
            "Use that result, or change the query.]",
            metadata={"deduped": True, "tool_name": name},
        )

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

    def _gated(self, name: str) -> ToolResult | None:
        """`gate` (set by the agent for a fix run) returns a message to refuse a call with, or None to let it through.
        Example: the first edit of a fix run is refused until the advisor has been consulted."""
        if not self.gate:
            return None
        msg = self.gate(name)
        return ToolResult.error_result(msg, metadata={"tool_name": name, "gated": True}) if msg else None

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

    def _cap_output(self, result: ToolResult, name: str = "") -> None:
        """One tool result must never be able to eat the context: keep the head
        and the tail (errors and summaries usually live at the end)."""
        if self.config.pi_mode:
            return self._pi_cap(result, name)
        limit = self.config.tool_output_max_chars
        out = result.output
        if len(out) > limit:
            head, tail = int(limit * 0.6), int(limit * 0.4)
            rid = self.archive.put(out)
            result.output = (
                out[:head]
                + f"\n... [{len(out) - limit} chars omitted; full output: {rid}, get a range with recall_result] ...\n"
                + out[-tail:]
            )
            result.truncated = True

    def _pi_cap(self, result: ToolResult, name: str) -> None:
        """Pi's limit: 2000 lines or 50KB, whichever is hit first, whole lines only. Shell output keeps its
        tail (the error is at the end); everything else keeps its head. The full text goes to a temp file the
        model can open, e.g. 'Showing last 2000 of 9120 lines. Full output: /tmp/mew-ab12.log ...'."""
        lines = result.output.split("\n")
        kept = lines[-PI_MAX_LINES:] if name == "shell" else lines[:PI_MAX_LINES]
        if name == "shell":
            kept = kept[::-1]  # walk from the end so the byte cut drops the oldest lines
        out, size = [], 0
        for line in kept:
            size += len(line.encode("utf-8")) + 1
            if size > (SHELL_MAX_BYTES if name == "shell" else PI_MAX_BYTES) and out:
                break
            out.append(line)
        if name == "shell":
            out.reverse()
        if len(out) == len(lines):
            return
        fd, full = tempfile.mkstemp(prefix="mew-", suffix=".log")
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(result.output)
        which = "last" if name == "shell" else "first"
        result.output = "\n".join(out) + f"\n\n[Showing {which} {len(out)} of {len(lines)} lines. Full output: {full} (outside the workspace, so page it with shell: sed -n '1,200p')]"
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

        veto = await hook_system.trigger_before_tool(name, params)
        if veto:
            result = ToolResult.error_result(veto, metadata={"tool_name": name, "blocked": True})
            await hook_system.trigger_after_tool(name, params, result)
            return result
        invocation = ToolInvocation(params=params, cwd=cwd, epoch=self.epoch, call_id=call_id, progress=self.progress)
        rule = approval_manager.rules.decide(name, params) if approval_manager else None
        if rule and rule.kind == "deny":
            result = ToolResult.error_result(
                f"Denied by the permission rule {rule.text}. Do not retry it or look for a way around it; tell the user what you needed.",
                metadata={"tool_name": name, "blocked": True},
            )
            await hook_system.trigger_after_tool(name, params, result)
            return result
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
                if rule and rule.kind == "allow" and decision == ApprovalDecision.NEEDS_CONFIRMATION:
                    decision = ApprovalDecision.APPROVED  # rejections (plan mode, dangerous commands) are never lifted by a rule
                elif rule and rule.kind == "ask" and decision == ApprovalDecision.APPROVED:
                    decision = ApprovalDecision.NEEDS_CONFIRMATION
                if decision == ApprovalDecision.REJECTED:
                    plan = approval_manager.approval_policy.value == "plan"
                    result = ToolResult.error_result(
                        "Plan mode is on: nothing may be changed. Finish investigating with read-only tools, then give the user a "
                        "concrete plan (files, edits, how to verify) and ask them to leave plan mode (/plan) so you can carry it out."
                        if plan else "Operation rejected by safety policy"
                    )
                    await hook_system.trigger_after_tool(name, params, result)
                    return result
                elif decision == ApprovalDecision.NEEDS_CONFIRMATION:
                    answer = await approval_manager.request_confirmation(confirmation)

                    if not answer.approved:
                        msg = "User rejected the operation."
                        if answer.feedback:
                            msg += f" {answer.feedback}"
                            _learn_from_rejection(cwd, name, params, answer.feedback)
                        result = ToolResult.error_result(
                            msg, metadata={"tool_name": name, "rejected": True, "no_approver": approval_manager.confirmation_callback is None}
                        )
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

        note = await hook_system.trigger_after_tool(name, params, result)
        if note:
            result.output = f"{result.output}\n\n{note}"
        return result


def create_default_registry(config: Config) -> ToolRegistry:
    registry = ToolRegistry(config)

    for tool_class in get_all_builtin_tools():
        registry.register(tool_class(config))
    registry.register(RecallResultTool(config, registry.archive))

    from tools.builtin.graph import GraphTool
    from tools.builtin.parallel_tasks import ParallelTasksTool
    from utils.codegraph import locate

    if config.enable_parallel_tasks and any((d / ".git").exists() for d in [config.cwd.resolve(), *config.cwd.resolve().parents][:6]):
        registry.register(ParallelTasksTool(config))

    # the tool needs either a graphify snapshot or a repo big enough for the live import graph to be worth a schema
    if locate(config.cwd) or _has_sources(config.cwd, 15):
        registry.register(GraphTool(config))

    for subagent_def in get_default_subagent_definitions(config):
        registry.register(SubagentTool(config, subagent_def))

    return registry


def _has_sources(cwd, at_least: int) -> bool:
    from utils.importgraph import _tracked

    return len(_tracked(cwd)) >= at_least


def _target_label(name: str, params: dict) -> str:
    """Short label for what a call looked at, for the token cost breakdown."""
    for key in ("symbol", "path", "pattern", "query", "command", "op", "goal"):
        v = params.get(key)
        if isinstance(v, str) and v:
            base = params.get("path") if key in ("symbol", "pattern") and isinstance(params.get("path"), str) else None
            return (f"{base}:{v}" if base else v)[:80]
    return ""


def _learn_from_rejection(cwd, tool: str, params: dict, feedback: str) -> None:
    """What the user said when they turned an action down becomes a lesson (listed by /lessons, removable by /forget)."""
    try:
        from agent.learn import Learned

        said = feedback.removeprefix("The user said:").strip()
        if said:
            Learned(cwd).add_lesson(f"After you tried {tool} {_target_label(tool, params)}, the user said: {said}", "rejection")
    except Exception:
        pass
