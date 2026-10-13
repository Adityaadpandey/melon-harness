from __future__ import annotations
import asyncio
import json
from typing import AsyncGenerator, Awaitable, Callable
from agent.events import AgentEvent, AgentEventType
from agent.session import Session
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

    async def run(self, message: str):
        await self.session.hook_system.trigger_before_agent(message)
        yield AgentEvent.agent_start(message)
        self.session.context_manager.add_user_message(message)

        final_response: str | None = None

        async for event in self._agentic_loop():
            yield event

            if event.type == AgentEventType.TEXT_COMPLETE:
                final_response = event.data.get("content")

        await self.session.hook_system.trigger_after_agent(message, final_response)
        yield AgentEvent.agent_end(final_response)

    async def _agentic_loop(self) -> AsyncGenerator[AgentEvent, None]:
        max_turns = self.config.max_turns

        for turn_num in range(max_turns):
            self.session.increment_turn()
            response_text = ""

            # check for context overflow
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
                elif event.type == StreamEventType.TOOL_CALL_COMPLETE:
                    if event.tool_call:
                        tool_calls.append(event.tool_call)
                elif event.type == StreamEventType.ERROR:
                    failed = True
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
                loop_prompt = create_loop_breaker_prompt(loop_detection_error)
                self.session.context_manager.add_user_message(loop_prompt)

            if usage:
                self.session.context_manager.set_latest_usage(usage)
                self.session.context_manager.add_usage(usage)

            self.session.context_manager.prune_tool_outputs()
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
