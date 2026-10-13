from __future__ import annotations
import asyncio
import json
from typing import AsyncGenerator, Awaitable, Callable
from agent.events import AgentEvent, AgentEventType
from agent.session import Session
from agent.task import TaskState, TaskStatus
from agent.verify import (
    changed_since,
    detect_verify_commands,
    repair_prompt,
    run_check,
    snapshot,
)
from client.response import StreamEventType, TokenUsage, ToolCall, ToolResultMessage
from config.config import Config
from prompts.system import create_loop_breaker_prompt
from tools.base import ToolConfirmation, ToolKind, ToolResult


class Agent:
    def __init__(
        self,
        config: Config,
        confirmation_callback: Callable[[ToolConfirmation], bool] | None = None,
    ):
        self.config = config
        self.session: Session | None = Session(self.config)
        self.session.approval_manager.confirmation_callback = confirmation_callback
        self.task: TaskState | None = None
        self._failed = False

    async def run(self, message: str):
        await self.session.hook_system.trigger_before_agent(message)
        yield AgentEvent.agent_start(message)
        self.session.context_manager.add_user_message(message)

        task = self.task = TaskState(
            request=message,
            tracer=self.session.tracer,
            max_seconds=self.config.max_seconds,
            max_repairs=self.config.max_repairs,
        )
        before = await snapshot(self.config.cwd)
        final_response: str | None = None

        while True:
            self._failed = False
            async for event in self._agentic_loop(task):
                yield event
                if event.type == AgentEventType.TEXT_COMPLETE:
                    final_response = event.data.get("content")

            if self._failed:
                task.transition(TaskStatus.FAILED, "agent loop aborted")
                break

            task.changed_files = changed_since(before, await snapshot(self.config.cwd))
            commands = self.config.verify_commands or (
                detect_verify_commands(self.config.cwd) if self.config.auto_verify else []
            )
            # outside git we can't tell what changed, so only verify when asked to
            if not task.changed_files and before is not None:
                task.transition(TaskStatus.DONE_NO_CHANGES)
                break
            if not commands:
                task.transition(TaskStatus.UNVERIFIED, "no verification commands")
                break

            task.transition(TaskStatus.VERIFYING, f"{len(commands)} check(s)")
            yield AgentEvent.notice(f"Verifying: {' && '.join(commands)}")
            results = []
            for cmd in commands:
                results.append(await run_check(cmd, self.config.cwd, self.config))
            task.verification = [r.to_dict() for r in results]
            failures = [r for r in results if not r.ok]
            if not failures:
                task.transition(TaskStatus.VERIFIED)
                break
            if task.repairs >= task.max_repairs or task.expired():
                task.transition(TaskStatus.VERIFICATION_FAILED, failures[0].command)
                break

            task.transition(TaskStatus.REPAIRING, failures[0].command)
            yield AgentEvent.notice(
                f"Verification failed ({failures[0].command}); repair attempt {task.repairs}/{task.max_repairs}"
            )
            self.session.context_manager.add_user_message(
                repair_prompt(failures, task.changed_files)
            )

        await self.session.hook_system.trigger_after_agent(message, final_response)
        yield AgentEvent.agent_end(final_response, report=task.report())

    async def _agentic_loop(self, task: TaskState) -> AsyncGenerator[AgentEvent, None]:
        max_turns = self.config.max_turns

        loop_warnings = 0
        for turn_num in range(max_turns):
            if task.expired():
                self._failed = True
                yield AgentEvent.agent_error(f"Time budget of {self.config.max_seconds:.0f}s exhausted")
                return
            self.session.increment_turn()
            response_text = ""

            # check for context overflow, against the model actually in use
            self.session.context_manager.context_window = (
                self.session.client.registry.get(self.config.model_name).context_length
            )
            if self.session.context_manager.needs_compression():
                summary, usage = await self.session.chat_compactor.compress(
                    self.session.context_manager
                )

                if summary:
                    self.session.context_manager.replace_with_summary(summary)
                    self.session.context_manager.set_latest_usage(usage)
                    self.session.context_manager.add_usage(usage)

            tool_schemas = self.session.tool_registry.get_schemas()

            tool_calls: list[ToolCall] = []
            usage: TokenUsage | None = None
            failed = False

            async for event in self.session.client.chat_completion(
                self.session.context_manager.get_messages(),
                tools=tool_schemas if tool_schemas else None,
            ):
                if event.type == StreamEventType.TEXT_DELTA:
                    if event.text_delta:
                        content = event.text_delta.content
                        response_text += content
                        yield AgentEvent.text_delta(content)
                elif event.type == StreamEventType.NOTICE:
                    yield AgentEvent.notice(event.notice or "")
                elif event.type == StreamEventType.TOOL_CALL_COMPLETE:
                    if event.tool_call:
                        tool_calls.append(event.tool_call)
                elif event.type == StreamEventType.ERROR:
                    failed = True
                    self._failed = True
                    yield AgentEvent.agent_error(
                        event.error or "Unknown error occurred.",
                    )
                    break
                elif event.type == StreamEventType.MESSAGE_COMPLETE:
                    usage = event.usage

            if failed:
                # a failed request must not leave a half-written assistant turn in history
                return

            self.session.context_manager.add_assistant_message(
                response_text or None,
                (
                    [
                        {
                            "id": tc.call_id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": json.dumps(tc.arguments),
                            },
                        }
                        for tc in tool_calls
                    ]
                    if tool_calls
                    else None
                ),
            )
            if response_text:
                yield AgentEvent.text_complete(response_text)
                self.session.loop_detector.record_action(
                    "response",
                    text=response_text,
                )

            if not tool_calls:
                if usage:
                    self.session.context_manager.set_latest_usage(usage)
                    self.session.context_manager.add_usage(usage)

                self.session.context_manager.prune_tool_outputs()
                return

            tool_call_results: list[ToolResultMessage] = []

            async for item in self._run_tool_calls(tool_calls):
                if isinstance(item, AgentEvent):
                    yield item
                else:
                    tool_call_results.append(item)

            for tool_result in tool_call_results:
                self.session.context_manager.add_tool_result(
                    tool_result.tool_call_id,
                    tool_result.content,
                )

            loop_detection_error = self.session.loop_detector.check_for_loop()
            if loop_detection_error:
                loop_warnings += 1
                if loop_warnings >= 3:
                    # the model was told twice and kept going; stop burning budget
                    self._failed = True
                    yield AgentEvent.agent_error(f"Stopped: {loop_detection_error}")
                    return
                loop_prompt = create_loop_breaker_prompt(loop_detection_error)
                self.session.context_manager.add_user_message(loop_prompt)

            if usage:
                self.session.context_manager.set_latest_usage(usage)
                self.session.context_manager.add_usage(usage)

            self.session.context_manager.prune_tool_outputs()
        self._failed = True
        yield AgentEvent.agent_error(f"Maximum turns ({max_turns}) reached")

    def _is_parallel_safe(self, tool_call: ToolCall) -> bool:
        tool = self.session.tool_registry.get(tool_call.name)
        return bool(tool) and tool.kind == ToolKind.READ and not tool.is_mutating(
            tool_call.arguments
        )

    async def _invoke(self, tool_call: ToolCall) -> ToolResult:
        return await self.session.tool_registry.invoke(
            tool_call.name,
            tool_call.arguments,
            self.config.cwd,
            self.session.hook_system,
            self.session.approval_manager,
        )

    async def _run_tool_calls(self, tool_calls: list[ToolCall]):
        """Run calls in model order. Runs of read-only calls (e.g. five greps
        in a row) execute concurrently; anything that writes or shells out
        acts as a barrier and runs alone, so edits never race with reads."""
        i = 0
        while i < len(tool_calls):
            j = i + 1
            if self._is_parallel_safe(tool_calls[i]):
                while j < len(tool_calls) and self._is_parallel_safe(tool_calls[j]):
                    j += 1
            batch = tool_calls[i:j]
            i = j

            for tc in batch:
                yield AgentEvent.tool_call_start(tc.call_id, tc.name, tc.arguments)
                self.session.loop_detector.record_action(
                    "tool_call", tool_name=tc.name, args=tc.arguments
                )

            results = await asyncio.gather(*(self._invoke(tc) for tc in batch))

            for tc, result in zip(batch, results):
                yield AgentEvent.tool_call_complete(tc.call_id, tc.name, result)
                yield ToolResultMessage(
                    tool_call_id=tc.call_id,
                    content=result.to_model_output(),
                    is_error=not result.success,
                )

    async def __aenter__(self) -> Agent:
        await self.session.initialize()
        return self

    async def __aexit__(
        self,
        exc_type,
        exc_val,
        exc_tb,
    ) -> None:
        if self.session and self.session.client and self.session.mcp_manager:
            await self.session.client.close()
            await self.session.mcp_manager.shutdown()
            self.session = None
