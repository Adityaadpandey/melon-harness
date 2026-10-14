from __future__ import annotations
import asyncio
import json
import time
from pathlib import Path
from typing import AsyncGenerator, Awaitable, Callable
from agent.events import AgentEvent, AgentEventType
from agent.root import pick_root
from agent.grounding import correction_prompt, has_file_code, verify_references, wants_change
from agent.learn import Learned
from agent.review import review_changes
from agent.routing_policy import classify
from agent.session import Session
from agent.skills import match_skills
from utils.cost import summarize_usage
from agent.task import TaskState, TaskStatus
from agent.verify import (
    backup,
    changed_since,
    repair_prompt,
    select_checks,
    run_check,
    snapshot,
)
from client.response import StreamEvent, StreamEventType, TokenUsage, ToolCall, ToolResultMessage
from config.config import Config
from prompts.system import create_loop_breaker_prompt
from tools.base import ToolConfirmation, ToolKind, ToolResult
from tools.builtin.shell import finished_jobs, kill_jobs


async def _within(seconds: float, stream):
    """Pass a model stream through, but give up when the request's time budget is
    gone: a stalled connection must not outlive the budget (retries and fallbacks
    each wait for their own read timeout)."""
    it = stream.__aiter__()
    deadline = time.monotonic() + seconds
    try:
        while True:
            left = deadline - time.monotonic()
            try:
                if left <= 0:
                    raise asyncio.TimeoutError
                yield await asyncio.wait_for(it.__anext__(), left)
            except StopAsyncIteration:
                return
            except asyncio.TimeoutError:
                yield StreamEvent(type=StreamEventType.ERROR, error="Time budget exhausted while waiting for the model")
                return
    finally:
        await it.aclose()


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
        self._tier = "default"
        self._escalated = False
        self._budget_hit = False
        self._budget_warned = False
        self._uncached_streak = 0
        self._usage_base = 0
        self._review_sec = 0.0
        self.undo_info: dict | None = None
        self._root_hint: Path | None = None  # the repo the last request worked in, when mew starts above it
        self._scout_error = ""

    def _adopt_root(self, root: Path | None) -> bool:
        """Work from the repo itself. The folder mew started in stays reachable for other repos next to it."""
        if root is None or root == self.config.cwd:
            return False
        self.config.allowed_dirs = [*self.config.allowed_dirs, str(self.config.cwd.resolve())]
        self.config.cwd = self._root_hint = root
        return True

    async def run(self, message: str):
        await self.session.hook_system.trigger_before_agent(message)
        if self._adopt_root(pick_root(self.config.cwd, message, self._root_hint)):
            yield AgentEvent.notice(f"Working in {self.config.cwd} (the repo this request is about)")
        yield AgentEvent.agent_start(message)
        playbooks = match_skills(self.session.skills, message)
        if playbooks:
            yield AgentEvent.notice("Loaded playbooks: " + ", ".join(p.name for p in playbooks))
            self.session.tracer.emit("skills_loaded", skills=[p.name for p in playbooks])
            loaded = "\n\n".join(f"### Playbook: {p.name}\n{p.body}" for p in playbooks)
            message_for_model = (
                f"{message}\n\n---\nPlaybooks selected for this request (follow them; "
                f"use `use_skill` with `section` for the full reference):\n\n{loaded}"
            )
        else:
            message_for_model = message
        self._scout_error = ""
        scout = await self._scout(message)
        if self._scout_error:
            yield AgentEvent.notice(f"Scout failed, continuing without it: {self._scout_error[:160]}")
        if scout:
            yield AgentEvent.notice("Scout finished; starting from its findings")
            message_for_model += (
                "\n\n---\nScout report (a cheaper read-only agent already searched; start from it, do not "
                "repeat its searches, and confirm any line you rely on before editing):\n" + scout
            )
        self.session.context_manager.add_user_message(message_for_model)

        self._escalated = False
        self._budget_hit = False
        self._budget_warned = False
        self._usage_base = self.session.context_manager.total_usage.total_tokens  # budget is per request
        self._review_sec = 0.0
        self._tier = "default"
        routing = self.config.routing
        if self.config.auto_route and not self.config.model_pinned and self.config.model_role == "default" and (
            "fast" in routing or "strong" in routing
        ):
            self._tier, why = classify(message)
            if self._tier in routing:
                chain = routing[self._tier][0]
                self.session.tracer.emit("route", tier=self._tier, reason=why, model=chain)
                yield AgentEvent.notice(f"Routing to the {self._tier} model ({why}): {chain}")
            else:
                self._tier = "default"

        task = self.task = TaskState(
            request=message,
            tracer=self.session.tracer,
            max_seconds=self.config.max_seconds,
            max_repairs=self.config.max_repairs,
        )
        before = await snapshot(self.config.cwd)
        backup_ref = await backup(self.config.cwd) if before is not None else None
        final_response: str | None = None
        nudged = False
        reviews = 0
        grounded_once = False

        while True:
            self._failed = False
            async for event in self._agentic_loop(task):
                yield event
                if event.type == AgentEventType.TEXT_COMPLETE:
                    final_response = event.data.get("content")

            if self._failed:
                task.transition(TaskStatus.FAILED, "agent loop aborted")
                break

            if self._budget_hit:
                task.transition(TaskStatus.UNVERIFIED, "token budget reached")
                break

            if final_response and self.config.check_references:
                n_refs, problems = await verify_references(final_response, self.config.cwd)
                task.grounding = {"checked": n_refs, "unverified": [f"{p.ref} ({p.why})" for p in problems]}
                self.session.tracer.emit("grounding", task=task.id, checked=n_refs, unverified=len(problems))
                if problems and not grounded_once:
                    grounded_once = True
                    yield AgentEvent.notice(f"{len(problems)} cited reference(s) could not be verified; asking for a correction")
                    self.session.context_manager.add_user_message(correction_prompt(problems))
                    continue

            if before is None:  # started above the repo and the request did not name it: the files touched tell
                targets = [e.get("target", "") for e in self.session.tracer.events if e["event"] == "tool_call"]
                if self._adopt_root(pick_root(self.config.cwd, message, self._root_hint, targets)):
                    yield AgentEvent.notice(f"Working in {self.config.cwd} (where the files you touched live)")
                    before = {}  # nothing known from before: every changed file counts as this request's
            task.changed_files = changed_since(before, await snapshot(self.config.cwd))
            checks = select_checks(self.config, task.changed_files)
            # outside git we can't tell what changed, so only verify when asked to
            if not task.changed_files and before is not None:
                if not nudged and final_response and wants_change(message) and has_file_code(final_response):
                    # models sometimes paste the solution instead of writing it
                    nudged = True
                    yield AgentEvent.notice("Reply contained code but no file changed; asking the model to apply it")
                    self.session.context_manager.add_user_message(
                        "You showed code but no file on disk changed. If the task needs "
                        "changes, apply them now with your tools. If no change is needed, "
                        "say so in one sentence."
                    )
                    continue
                task.transition(TaskStatus.DONE_NO_CHANGES)
                break
            failures: list = []
            if checks:
                task.transition(TaskStatus.VERIFYING, f"{len(checks)} check(s)")
                yield AgentEvent.notice(f"Verifying: {' && '.join(c for c, _ in checks)}")
                results = []
                for cmd, sub in checks:
                    results.append(await run_check(cmd, self.config.cwd / sub, self.config))
                task.verification = [r.to_dict() for r in results]
                failures = [r for r in results if not r.ok]

            if failures:
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
                continue

            # checks passed (or none exist): an independent reviewer reads the real diff
            if self.config.auto_review and reviews < self.config.max_reviews and not task.expired():
                reviews += 1
                yield AgentEvent.notice("Independent review of the diff...")
                t_review = time.monotonic()
                res = await review_changes(self.config, message, task.changed_files, task.verification, final_response or "")
                self._review_sec += time.monotonic() - t_review
                task.review = {"verdict": res.verdict, "findings": res.findings}
                self.session.tracer.emit("review", task=task.id, verdict=res.verdict, findings=res.findings[:1500])
                if res.verdict == "ISSUES" and task.repairs < task.max_repairs:
                    task.transition(TaskStatus.REPAIRING, "review findings")
                    yield AgentEvent.notice("Reviewer found issues; sending them back for a fix")
                    self.session.context_manager.add_user_message(
                        "An independent reviewer examined your diff and reported these problems:\n\n"
                        f"{res.findings}\n\nFix each one that is real. If a finding is wrong, say why in one "
                        "sentence and leave the code alone. Keep changes minimal, then stop."
                    )
                    continue

            if checks:
                task.transition(TaskStatus.VERIFIED)
            else:
                task.transition(TaskStatus.UNVERIFIED, "no verification commands")
            break

        if task.status == TaskStatus.VERIFIED and task.changed_files and (task.review or {}).get("verdict") != "ISSUES":
            try:
                Learned(self.config.cwd).record_run(message, task.changed_files)  # which files fixed what, for next time
            except Exception:
                pass
        if backup_ref and task.changed_files:
            created = [f for f in task.changed_files if f not in (before or {})]
            self.undo_info = {"ref": backup_ref, "files": list(task.changed_files), "created": created}
        await self.session.hook_system.trigger_after_agent(message, final_response)
        report = task.report()
        report["undo"] = bool(self.undo_info)
        report["usage"] = summarize_usage(self.session.tracer.events, self.config.pricing)
        ev = self.session.tracer.events
        report["timings"] = {
            "wall_sec": round(time.monotonic() - task.started, 1),
            "model_sec": round(sum(e.get("sec", 0) for e in ev if e["event"] == "model_call" and e.get("ok")), 1),
            "tool_sec": round(sum(e.get("sec", 0) for e in ev if e["event"] == "tool_call"), 1),  # parallel calls overlap, so this can exceed wall
            "verify_sec": round(sum(c.get("seconds", 0) for c in task.verification), 1),
            "review_sec": round(self._review_sec, 1),
        }
        yield AgentEvent.agent_end(final_response, report=report)

    async def _agentic_loop(self, task: TaskState) -> AsyncGenerator[AgentEvent, None]:
        max_turns = self.config.max_turns

        loop_warnings = 0
        for turn_num in range(max_turns):
            if task.expired():
                self._failed = True
                yield AgentEvent.agent_error(f"Time budget of {self.config.max_seconds:.0f}s exhausted")
                return
            used = self.session.context_manager.total_usage.total_tokens - self._usage_base
            budget = self.config.max_total_tokens
            if used > budget:
                async for ev in self._wrap_up(task, f"Token budget reached ({budget:,})", "Token budget is spent"):
                    yield ev
                return
            if used > 0.7 * budget and not self._budget_warned:
                self._budget_warned = True
                self.session.context_manager.add_user_message(
                    "Budget check: about 70% of the token budget is used. Stop broad searching. Read only what "
                    "you must, then answer or make the change."
                )
            for job in finished_jobs():
                state = "finished" if job.process.returncode == 0 else f"failed (exit {job.process.returncode})"
                took = time.monotonic() - job.started
                yield AgentEvent.notice(f"Background job {job.id} {state} in {took:.0f}s: {job.command}")
                self.session.context_manager.add_user_message(
                    f"[background job {job.id} `{job.command}` {state}; last output:]\n{job.tail()}"
                )
            self.session.increment_turn()
            response_text = ""

            # check for context overflow, against the model actually in use
            self.session.context_manager.context_window = (
                self.session.client.registry.get(self.config.model_name).context_length
            )
            if self.session.context_manager.needs_compression():
                if await self._compact():
                    yield AgentEvent.notice("Context compacted (recent messages kept)")

            tool_schemas = self.session.tool_registry.get_schemas()

            tool_calls: list[ToolCall] = []
            usage: TokenUsage | None = None
            failed = False

            async for event in _within(task.remaining(), self.session.client.chat_completion(
                self.session.context_manager.get_messages(),
                tools=tool_schemas if tool_schemas else None,
                role=self._role(task),
            )):
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

                if self.session.context_manager.prune_tool_outputs():
                    self.session.tool_registry.epoch += 1
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
                self._escalated = True  # stuck: bring in the strong model
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
                # big prompts that never hit the cache are re-billed in full every call
                self._uncached_streak = self._uncached_streak + 1 if usage.prompt_tokens > 15_000 and not usage.cached_tokens else 0
                if self._uncached_streak == 3:
                    yield AgentEvent.notice(
                        f"{self.config.model_name} is not reusing its prompt cache (3 large calls, 0 cached tokens). "
                        "Every call re-bills the whole context; a Claude or deepseek model is much cheaper here."
                    )

            if self.session.context_manager.prune_tool_outputs():
                self.session.tool_registry.epoch += 1
        async for ev in self._wrap_up(task, f"Turn limit reached ({max_turns})", "Turn limit reached"):
            yield ev

    async def _scout(self, message: str) -> str:
        """Search in a throwaway read-only context so the main (pricier, ever-growing)
        context starts from findings instead of 30 grep results."""
        reg = self.session.tool_registry
        if not (self.config.auto_scout and self.config.model_role == "default" and reg.get("subagent_investigator")):
            return ""
        if len(message) < 80 or classify(message)[0] == "fast":
            return ""
        self.session.tracer.emit("scout", goal=message[:200])
        res = await reg.invoke(
            "subagent_investigator", {"goal": message}, self.config.cwd, self.session.hook_system, self.session.approval_manager
        )
        if not res.success:
            self._scout_error = (res.error or "unknown error").splitlines()[0]
            return ""
        return (res.output or "")[:6000]

    async def _wrap_up(self, task: TaskState, notice: str, why: str) -> AsyncGenerator[AgentEvent, None]:
        """Out of budget or turns: one last tool-free call, so the work done so far
        becomes an answer (or a verdict, for a reviewer) instead of a bare failure."""
        self._budget_hit = True
        yield AgentEvent.notice(f"{notice}; wrapping up with what is known")
        self.session.context_manager.add_user_message(
            f"{why}. No more tool calls. Answer now from what you have already read: what you found (file:line), "
            "what is still unknown, and the next step. If you were asked for a specific reply format, use it."
        )
        text = ""
        async for event in self.session.client.chat_completion(
            self.session.context_manager.get_messages(), tools=None, role=self._role(task)
        ):
            if event.type == StreamEventType.TEXT_DELTA and event.text_delta:
                text += event.text_delta.content
                yield AgentEvent.text_delta(event.text_delta.content)
            elif event.type == StreamEventType.MESSAGE_COMPLETE and event.usage:
                self.session.context_manager.add_usage(event.usage)
        self.session.context_manager.add_assistant_message(text or None, None)
        if text:
            yield AgentEvent.text_complete(text)

    def _role(self, task: TaskState) -> str:
        """Routing role for the next model call: a fixed role for sub-agents and
        reviewers, otherwise the request's tier, escalated to `strong` after a
        failed repair or a loop (if a strong chain is configured)."""
        if self.config.model_role != "default":
            return self.config.model_role
        tier = self._tier
        if (task.repairs >= 1 or self._escalated) and "strong" in self.config.routing:
            tier = "strong"
        return tier

    async def _compact(self) -> bool:
        cm = self.session.context_manager
        summary, usage, cut = await self.session.chat_compactor.compress(cm)
        if not summary:
            return False
        before = cm._latest_usage.total_tokens
        self.session.tool_registry.epoch += 1  # earlier reads are no longer in context
        cm.apply_compaction(summary, cut)
        cm.set_latest_usage(usage)
        cm.add_usage(usage)
        self.session.tracer.emit("compaction", cut=cut, tokens_before=before, summary_tokens=usage.completion_tokens if usage else None)
        return True

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
            tool_call.call_id,
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
        kill_jobs()
        if self.session and self.session.client and self.session.mcp_manager:
            await self.session.client.close()
            await self.session.mcp_manager.shutdown()
            self.session = None
