from __future__ import annotations
import asyncio
import json
import re
import time
from pathlib import Path
from typing import AsyncGenerator, Awaitable, Callable
from agent.challenge import challenge_request, is_risky, render as render_objections
from agent.brevity import HOLD_CHARS, LONG_ANSWER_WORDS, interim_line
from agent.events import AgentEvent, AgentEventType
from agent.root import pick_root
from agent.grounding import REWRITE_PROMPT, correction_prompt, has_file_code, shows_second_thoughts, verify_references, wants_change
from agent.learn import Learned
from agent import review_fix
from agent.review_pipeline import run_review_pipeline
from agent.review import ReviewResult, merge_reviews, review_changes
from agent.review_fix_verify import verify_fix
from agent.routing_policy import EFFORT, REVIEWY, classify, length_hint
from agent.scope import SWEEP, SWEEP_CHECKPOINT, is_broad, unfinished
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
from config.config import Config, HookTrigger
from config.loader import get_data_dir
from prompts.system import create_loop_breaker_prompt
from tools.base import ToolConfirmation, ToolKind, ToolResult
from tools.builtin.shell import finished_jobs, kill_jobs
from utils.paths import resolve_path


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


OVERFLOW = re.compile(r"context[_ ](length|window)|maximum context|prompt is too long|too many tokens|exceeds? the (model|context)|input (is )?too long", re.I)


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
        self.steering: list[str] = []  # typed while the agent works; lands after the current round of tool calls
        self._failed = False
        self._tier = "default"
        self._escalated = False
        self._budget_hit = False
        self._compact_retry_turn = 0
        self._overflow_retried = False
        self._last_model_call = 0.0
        self.capped_last = False  # did the last request stop at the call budget? /more only makes sense then
        self.extra_calls = 0  # set by /more, consumed by the next request
        self._allowance = 0
        self._calls_used = 0
        self._calls_warned = False
        self._shown: list = []  # findings of the last review, in the order the answer numbered them
        self._fix_run = False
        self._fix_brief = ""
        self._read_only = False
        self._todos_reminded = ""
        self._pipelined = False
        self._pipeline_answer = ""
        self._budget_warned = False
        self._uncached_streak = 0
        self._usage_base = 0
        self._review_sec = 0.0
        self.undo_info: dict | None = None
        self._long_answer = 0  # words in the last final answer when it ran over the limit; reminded on the next request
        self.checkpoints: list[dict] = []  # one entry per request that changed files, for /rewind
        self._root_hint: Path | None = None  # the repo the last request worked in, when mew starts above it
        self._scout_error = ""
        self._no_approver = False
        self._playbooks_sent: set[str] = set()  # names whose text is in the conversation; cleared on compaction
        self._unsent = ""  # the request while the review pipeline, scout or challenger runs, before it is in the history

    def on_interrupt(self) -> None:
        """Ctrl+C. A request cut off while the review pipeline was still running never reached the history, so the next
        "pls cont" found nothing to continue. Keep it, marked as interrupted, so the model can pick it back up."""
        cm = self.session.context_manager
        if self._unsent:
            cm.add_user_message(self._unsent + "\n\n[Interrupted by the user before you answered (the review, scout or challenger was still running). Nothing was changed.]")
            self._unsent = ""
        cm.repair_orphans()  # providers reject unanswered tool calls

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
        self._unsent = message
        matched = match_skills(self.session.skills, message)
        # a playbook already in the conversation is named, not pasted again (the code-review one is ~1.5k tokens a request)
        playbooks = [p for p in matched if p.name not in self._playbooks_sent]
        again = [p.name for p in matched if p.name in self._playbooks_sent]
        self._playbooks_sent.update(p.name for p in playbooks)
        if playbooks:
            yield AgentEvent.notice("Loaded playbooks: " + ", ".join(p.name for p in playbooks))
            self.session.tracer.emit("skills_loaded", skills=[p.name for p in playbooks])
            loaded = "\n\n".join(f"### Playbook: {p.name}\n{p.body}" for p in playbooks)
            # a playbook's long reference is pulled with use_skill; pi mode has no such tool, so it is not offered
            reference = "; use `use_skill` with `section` for the full reference" if any(t.name == "use_skill" for t in self.session.tool_registry.get_tools()) else ""
            message_for_model = f"{message}\n\n---\nPlaybooks selected for this request (follow them; they are already below, so do not read their files again{reference}):\n\n{loaded}"
        else:
            message_for_model = message
        if again:
            message_for_model += f"\n\n[Playbooks given earlier in this conversation still apply: {', '.join(again)}.]"
        if self._long_answer:
            message_for_model += f"\n\n[Reminder: your last final answer ran {self._long_answer} words. Keep this one to the final-answer limit (120 words, 6 bullets) unless the user asked for detail.]"
            self._long_answer = 0
        self._pipelined = False
        self._pipeline_answer = ""
        self._fix_run, self._fix_brief = False, ""
        if self._shown and not REVIEWY.search(message) and review_fix.asks_for_fix(message):
            picked = review_fix.chosen(message, self._shown)  # "fix them" after a review: those findings, with everything the review learned
            if picked:
                self._fix_run, self._fix_brief = True, review_fix.short(picked)
                message_for_model += "\n\n---\n" + review_fix.brief(picked)
                yield AgentEvent.notice(f"Fixing {len(picked)} finding(s) from the last review: advisor first, then the fix, then an independent check")
        if self.config.review_pipeline and self.config.model_role == "default" and REVIEWY.search(message):
            results = ""
            async for kind, value in run_review_pipeline(self.config, message):
                if kind == "notice":
                    yield AgentEvent.notice(value)
                elif kind == "answer":
                    self._pipeline_answer = value
                elif kind == "findings":
                    self._shown = value  # what "fix 2" will point at
                else:
                    results = value
            self._pipelined = bool(results)
            if results and review_fix.review_and_fix(message) and (picked := review_fix.chosen("", self._shown)):
                self._fix_run, self._fix_brief = True, review_fix.short(picked)
                message_for_model += "\n\n---\n" + review_fix.brief(picked)
                yield AgentEvent.notice(f"Fixing the {len(picked)} proven finding(s): advisor first, then the fix, then an independent check")
            if results:
                message_for_model += (
                    "\n\n---\nReview results. Each claim below was checked by a separate agent that read the code and tried to refute it. "
                    "Answer from them with no headings and none of the bracket tags: confirmed items as `severity file:line  what is wrong -> fix`; "
                    "unverified items the same with (unverified) after the severity, at most MEDIUM; rejected items are not findings and are not mentioned. "
                    "One finding per line. "
                    "If the user also asked for a fix, fix the confirmed items first, then check once. If nothing is confirmed or unverified, say: No issues found.\n" + results
                )
        self._scout_error = ""
        self._no_approver = False
        scout = await self._scout(message)
        if self._scout_error:
            yield AgentEvent.notice(f"Scout failed, continuing without it: {self._scout_error[:160]}")
        if scout:
            yield AgentEvent.notice("Scout finished; starting from its findings")
            message_for_model += (
                "\n\n---\nScout report (a cheaper read-only agent already searched; start from it, do not "
                "repeat its searches, and confirm any line you rely on before editing):\n" + scout
            )
        if self.config.auto_challenge and is_risky(message):
            yield AgentEvent.notice("Challenger is questioning this request first")
            objections, dropped, err = await challenge_request(self.config, message)
            if err:
                yield AgentEvent.notice(f"Challenger failed, continuing without it: {err[:160]}")
            elif objections:
                notes = render_objections(objections, dropped)
                yield AgentEvent.notice("Challenger objections:\n" + notes)
                message_for_model += (
                    "\n\n---\nChallenger objections (a second model argued against this request). [verified] ones are "
                    "facts to deal with; [guess] ones are suspicions to check with your tools, never to repeat as fact. "
                    "Before editing, say which objections you accept and which you reject and why, and tell the user "
                    "about any real loss in their approach:\n" + notes
                )
            else:
                yield AgentEvent.notice("Challenger found nothing it could back with evidence")
        async for ev in self._tidy_if_cache_went_cold():
            yield ev
        difficulty = classify(message)[0]
        self.session.client.auto_effort = EFFORT[difficulty]
        if hint := length_hint(message, difficulty, wants_change(message)):
            message_for_model += f"\n\n[{hint}]"
        self._broad = wants_change(message) and is_broad(message)
        if self._broad:
            message_for_model += f"\n\n[{SWEEP}]"
        self._sweep_pushed = 0
        self.session.context_manager.add_user_message(message_for_model)
        self._unsent = ""

        self._escalated = False
        self._budget_hit = False
        self._budget_warned = False
        self._overflow_retried = False
        self._calls_used = 0
        self._calls_warned = False
        self._quiet_rounds = 0  # rounds in a row where the model said nothing, only called tools
        self._edited = False
        self._fix_nudged = False
        self.capped_last = False
        self._allowance, self.extra_calls = self.extra_calls, 0  # /more applies to this one request
        self._read_only = not wants_change(message)
        self._wants_change = not self._read_only
        self._is_review = bool(REVIEWY.search(message))
        self._usage_base = self._spent()  # budget is per request
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
        registry = self.session.tool_registry
        advisor = registry.get("advisor") if any(t.name == "advisor" for t in registry.get_tools()) else None
        if advisor:
            advisor.calls = 0  # the limit is per request
        registry.gate = review_fix.edit_gate(registry, advisor) if self._fix_run and advisor else None  # a fix run may not edit before the advisor has been consulted
        base_writes, advisor_nudges = registry._writes, 0
        self._waited_base = self.session.approval_manager.waited
        before = await snapshot(self.config.cwd)
        backup_ref = await backup(self.config.cwd) if before is not None else None
        final_response: str | None = None
        nudged = False
        reviews = 0
        sent_back = self._fix_brief  # findings the author was just asked to fix: the next review checks that fix (a fix run starts with the review's own)
        grounded_once = False
        rewrote = False
        stop_blocks = 0  # times a stop hook sent the work back; capped so a hook cannot loop forever

        while True:
            self._failed = False
            async for event in self._agentic_loop(task):
                yield event
                if event.type == AgentEventType.TEXT_COMPLETE:
                    final_response = event.data.get("content")

            if self._failed or self._budget_hit:
                # a run cut short may still have edited files; the report must say which
                task.changed_files = changed_since(before, await snapshot(self.config.cwd))

            if self._failed:
                task.transition(TaskStatus.FAILED, "agent loop aborted")
                break

            if self._budget_hit:
                # the checks cost no model tokens, so a run that ran out of budget mid-change still finds out if it broke the tree
                checks = select_checks(self.config, task.changed_files) if task.changed_files else []
                results = [await run_check(cmd, self.config.cwd / sub, self.config) for cmd, sub in checks]
                task.verification = [r.to_dict() for r in results]
                bad = [r for r in results if not r.ok]
                if bad:
                    yield AgentEvent.notice(f"Stopped on the budget and the tree fails {bad[0].command}; the change is incomplete")
                    task.transition(TaskStatus.VERIFYING, "checks after budget stop")
                    task.transition(TaskStatus.VERIFICATION_FAILED, bad[0].command)
                else:
                    task.transition(TaskStatus.UNVERIFIED, "token budget reached")
                break

            if final_response and self.config.check_references:
                n_refs, problems = await verify_references(final_response, self.config.cwd)
                task.grounding = {"checked": n_refs, "unverified": [f"{p.ref} ({p.why})" for p in problems]}
                self.session.tracer.emit("grounding", task=task.id, checked=n_refs, unverified=len(problems))
                # after a call-cap wrap-up there are no tools left to re-check with, so a retry would only pay for a second full-context answer
                if problems and not grounded_once and not self.capped_last:
                    grounded_once = True
                    yield AgentEvent.notice(f"{len(problems)} cited reference(s) could not be verified; asking for a correction")
                    self.session.context_manager.add_user_message(correction_prompt(problems))
                    continue

            if final_response and not rewrote and not self._pipelined and shows_second_thoughts(message, final_response):
                rewrote = True
                yield AgentEvent.notice("The review argues with itself; asking for a clean rewrite")
                self.session.context_manager.add_user_message(REWRITE_PROMPT)
                # a plain tool-free answer: re-entering the loop would hit the call cap and ask for a "next step" summary instead
                async for ev in self._answer_without_tools(task):
                    yield ev
                    if ev.type == AgentEventType.TEXT_COMPLETE:
                        final_response = ev.data.get("content")

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
                if stop_blocks < 2 and (held := await self.session.hook_system.trigger_stop(message, final_response or "")):
                    stop_blocks += 1
                    yield AgentEvent.notice("A stop hook says the work is not finished; sending it back")
                    self.session.context_manager.add_user_message(held)
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

            if self._fix_run and advisor and advisor_nudges < 2 and not task.expired() and review_fix.needs_final_advice(registry, advisor, base_writes):
                advisor_nudges += 1
                yield AgentEvent.notice("The fix changed code since the advisor last looked; asking for a final consultation")
                self.session.context_manager.add_user_message(review_fix.ADVISOR_LAST)
                continue

            # checks passed (or none exist): an independent reviewer reads the real diff
            max_reviews = max(self.config.max_reviews, 3) if self._fix_run else self.config.max_reviews  # a fix gets review, repair, re-review
            if (self.config.auto_review or self._fix_run) and reviews < max_reviews and not task.expired():
                reviews += 1
                yield AgentEvent.notice("Independent review of the diff...")
                t_review = time.monotonic()
                res = await review_changes(self.config, message, task.changed_files, task.verification, final_response or "", **({"fixing": sent_back} if sent_back else {}))
                if self.config.second_reviewer and res.verdict in ("PASS", "ISSUES"):
                    yield AgentEvent.notice("Second reviewer looking for what the first missed...")
                    second = await review_changes(self.config, message, task.changed_files, task.verification, final_response or "", prior=res.findings or "VERDICT: PASS", role="review2")
                    res = merge_reviews(res, second)
                if self._fix_run and not task.expired():
                    yield AgentEvent.notice("A fresh verifier is trying to prove the fix works (it runs the checks and the findings' triggers in a copy)")
                    fv = await verify_fix(self.config, self.config.review_pipeline_role, self.config.cwd, self._fix_brief, final_response or "", task.changed_files,
                                          ", ".join(f"{c['command']}={'ok' if c['ok'] else 'FAILED'}" for c in task.verification))
                    if fv.ran:
                        yield AgentEvent.notice(f"Fix verifier: {'PASS' if fv.passed else 'FAIL'} ({fv.summary()})")
                        if not fv.passed:
                            res = ReviewResult("ISSUES", (res.findings + "\n\n" if res.verdict == "ISSUES" else "") + "A fresh verifier ran the fix and found:\n" + fv.feedback())
                self._review_sec += time.monotonic() - t_review
                if res.blocked:
                    yield AgentEvent.notice(f"The reviewer had {res.blocked} tool call(s) blocked by hooks or permission rules, so its check was narrower than usual")
                task.review = {"verdict": res.verdict, "findings": res.findings}
                self.session.tracer.emit("review", task=task.id, verdict=res.verdict, findings=res.findings[:1500])
                if res.verdict == "ISSUES" and reviews >= max_reviews:
                    # the fix itself was reviewed and still has problems: another blind round costs ~50 calls, so the user decides
                    yield AgentEvent.notice("The review of the fix still found problems (left unfixed):\n" + res.findings)
                elif res.verdict == "ISSUES" and task.repairs < task.max_repairs:
                    sent_back = res.findings
                    task.transition(TaskStatus.REPAIRING, "review findings")
                    yield AgentEvent.notice("Reviewer found issues; sending them back for a fix")
                    self.session.context_manager.add_user_message(
                        "An independent reviewer examined your diff and reported these problems:\n\n"
                        f"{res.findings}\n\nFix every finding that is real and sits in code this diff added or changed, whatever its severity: a dead control or a "
                        "blank page that your change caused is a regression, not a nit. Only defer a finding about code you did not touch, and list it. "
                        "If a finding is wrong, say why in one sentence and leave the code alone. After the fix, run what proves it (the page, request or test the "
                        "finding names), not only the typecheck."
                    )
                    continue

            if stop_blocks < 2 and not task.expired() and (held := await self.session.hook_system.trigger_stop(message, final_response or "")):
                stop_blocks += 1
                yield AgentEvent.notice("A stop hook says the work is not finished; sending it back")
                self.session.context_manager.add_user_message(held)
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
            self.checkpoints = [*self.checkpoints, {**self.undo_info, "request": message[:80]}][-20:]
        if final_response and len(final_response.split()) > LONG_ANSWER_WORDS and not REVIEWY.search(message):
            self._long_answer = len(final_response.split())
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
        if self._pipelined and self._read_only:
            # the reviewer and verifier already read the code, and each finding is already one line: no model call, no tool loop to wander in
            text = self._pipeline_answer
            self.session.context_manager.add_assistant_message(text, None)
            yield AgentEvent.text_delta(text)
            yield AgentEvent.text_complete(text)
            return
        max_turns = self.config.max_turns

        loop_warnings = 0
        for turn_num in range(max_turns):
            # minutes spent on an approval prompt are the user's, not the agent's: they never use up the budget
            task.paused = self.session.approval_manager.waited - self._waited_base
            if task.expired():
                # out of time: answer from what is known instead of failing with nothing to show
                async for ev in self._wrap_up(task, f"Time budget reached ({self.config.max_seconds:.0f}s)", "The time budget is spent"):
                    yield ev
                return
            limit = self._call_limit()
            if limit and self._calls_used >= limit:
                async for ev in self._wrap_up(
                    task, f"Call budget reached ({limit} tool rounds)",
                    f"The {limit}-round budget for this request is spent (the user can type /more to allow 10 more)",
                ):
                    yield ev
                self._budget_hit = False  # the answer was produced; this is not a token-budget failure
                self.capped_last = True
                return
            if limit and self._calls_used >= max(1, int(limit * 0.7)) and not self._calls_warned:
                self._calls_warned = True
                if self._is_review:
                    # unread candidates turn into guessed HIGHs at the cap: spend what is left on the best two
                    warning = (
                        f"Budget check: {self._calls_used} of {limit} tool rounds used. Pick the two candidates most likely to be real defects "
                        "and use the remaining rounds, in parallel, to read the code that proves or refutes each. Drop the rest: an unread "
                        "candidate is not a finding. Keep any notes to 5 lines (candidate, file:line, checked or not; no severity)."
                    )
                elif not self._read_only:
                    warning = (
                        f"Budget check: {self._calls_used} of {limit} tool rounds used. Fix the HIGH items first, batch the remaining "
                        "edits and checks into parallel calls, and list anything you will not get to so the user can decide."
                    )
                else:
                    warning = (
                        f"Budget check: {self._calls_used} of {limit} tool rounds used. Put every lookup you still need into "
                        "your next turn as parallel calls, then answer. Before reading more, jot short working notes "
                        "(candidate, file:line, checked or not; no severity, nothing called verified until you read the code that proves it): "
                        "old tool results get cleared, your own text stays. If the task asks for an output file, write it now, then refine it."
                    )
                self.session.context_manager.add_user_message(warning)
            if self._quiet_rounds >= 8:
                # reasoning is dropped between turns and old tool results get cleared: only written text survives
                self._quiet_rounds = 0
                self.session.context_manager.add_user_message(
                    SWEEP_CHECKPOINT if self._broad else
                    "Checkpoint: you have looked things up for several rounds without writing anything down. Before the next lookup, "
                    "jot working notes in at most 3 lines (candidate, file:line, checked or not; no severity) and what you will check next. If you have verified a defect, fix it now."
                )
            elif self._wants_change and not self._broad and not self._edited and not self._fix_nudged and self._calls_used >= 15:
                self._fix_nudged = True
                self.session.context_manager.add_user_message(
                    f"{self._calls_used} rounds in and nothing is changed yet. If you have a verified finding, name your root cause in one line "
                    "and make the smallest fix for it (ask first only if it adds or restyles something the user will see; a guard, a prop or a handler change needs no asking). "
                    "Do not add new files or components just to have a change. If you have no cause, say what you traced and why it holds."
                )
            used = self._spent() - self._usage_base
            budget = self.config.max_total_tokens
            if used > budget:
                async for ev in self._wrap_up(task, f"Token budget reached ({budget:,})", "Token budget is spent"):
                    yield ev
                return
            if used > 0.7 * budget and not self._budget_warned:
                self._budget_warned = True
                self.session.context_manager.add_user_message(
                    "Budget check: about 70% of the token budget is used. Stop broad searching. Jot short "
                    "working notes (candidate, file:line, checked or not; no severity), since old tool results get cleared. If the task asks "
                    "for an output file, write it now. Read only what you must, then answer or make the change."
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
            if self.session.context_manager.needs_compression() and self.session.turn_count >= self._compact_retry_turn:
                if await self._compact():
                    yield AgentEvent.notice("Context compacted (recent messages kept)")
                else:
                    # the summarizer failed: shed old tool output instead, and don't pay for another
                    # failing summary call on every turn (try again in 3 turns)
                    self._compact_retry_turn = self.session.turn_count + 3
                    self.session.context_manager.prune_tool_outputs()
                    yield AgentEvent.notice("Could not summarise the context; pruned old tool output instead")

            tool_schemas = self.session.tool_registry.get_schemas()

            tool_calls: list[ToolCall] = []
            usage: TokenUsage | None = None
            failed = False
            overflowed = False
            held, streaming = "", False  # text waits until we know whether tool calls follow (then it shows as one line)

            async for event in _within(task.remaining(), self.session.client.chat_completion(
                self.session.context_manager.get_messages(),
                tools=tool_schemas if tool_schemas else None,
                role=self._role(task),
            )):
                if event.type == StreamEventType.TEXT_DELTA:
                    if event.text_delta:
                        content = event.text_delta.content
                        response_text += content
                        if streaming:
                            yield AgentEvent.text_delta(content)
                        else:
                            held += content
                            if len(held) > HOLD_CHARS:
                                streaming = True
                                yield AgentEvent.text_delta(held)
                                held = ""
                elif event.type == StreamEventType.NOTICE:
                    yield AgentEvent.notice(event.notice or "")
                elif event.type == StreamEventType.TOOL_CALL_COMPLETE:
                    if event.tool_call:
                        tool_calls.append(event.tool_call)
                elif event.type == StreamEventType.ERROR:
                    if OVERFLOW.search(event.error or "") and not self._overflow_retried:
                        overflowed = True  # the provider says the prompt is too big: compact and ask again, once
                        break
                    failed = True
                    self._failed = True
                    await self.session.hook_system.fire(HookTrigger.ON_ERROR, error=event.error or "")
                    yield AgentEvent.agent_error(
                        event.error or "Unknown error occurred.",
                    )
                    break
                elif event.type == StreamEventType.MESSAGE_COMPLETE:
                    usage = event.usage

            if held and not overflowed:
                yield AgentEvent.text_delta(interim_line(held) if tool_calls else held)

            if overflowed:
                self._overflow_retried = True
                yield AgentEvent.notice("The model refused the prompt as too long; compacting and retrying")
                if await self._compact():
                    continue
                self.session.context_manager.prune_tool_outputs()
                yield AgentEvent.notice("Could not summarise; pruned old tool output and retrying")
                continue

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

            self._quiet_rounds = 0 if response_text else self._quiet_rounds + 1
            self._edited = self._edited or any(tc.name in {"edit", "write_file"} for tc in tool_calls)
            self._calls_used += 1  # one round trip however many calls it holds: reading 5 files at once costs what reading 1 does
            if any(tc.name in {"edit", "write_file"} for tc in tool_calls):
                self._read_only = False  # it turned out to be a change after all: the read cap no longer applies
            if not tool_calls:
                if usage:
                    self.session.context_manager.set_latest_usage(usage)
                    self.session.context_manager.add_usage(usage)

                left = unfinished(self.session.tool_registry.get("todos")) if self._broad else []
                if left and self._sweep_pushed < 2:
                    # a sweep that ends with open steps stops after the first few files, which is what this guards against
                    self._sweep_pushed += 1
                    self.session.context_manager.add_user_message(
                        f"{len(left)} step(s) on your list are still open: " + "; ".join(left[:6])
                        + ". Finish them (fix, or mark done with a one-line reason it is fine) before you answer."
                    )
                    continue
                self._prune()
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

            while self.steering:
                note = self.steering.pop(0)
                self.session.context_manager.add_user_message(f"[the user added, while you were working:] {note}")
                yield AgentEvent.notice(f"steering received: {note[:80]}")

            if self._no_approver:
                # nobody can answer the approval prompt in this run, so retrying cannot succeed: stop instead of burning the budget
                self._failed = True
                yield AgentEvent.agent_error(
                    "Stopped: this change needs approval and the run cannot ask. Re-run with --approval auto-edit "
                    "(edits) or --approval auto (edits and commands), or start mew interactively."
                )
                return

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

            self._last_model_call = time.monotonic()
            self._prune(remind=True)
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
        if self._is_review:
            ask = (
                "Answer now with the review itself: only defects you verified by reading the code, as `severity file:line  what is wrong -> fix`, "
                "at most 6. Leave out anything you retracted, could not verify, or that is redundancy or style; a guess is `(unverified)` and at most "
                "MEDIUM. At most one closing line, for a check you did not run."
            )
        else:
            ask = (
                "Answer now from what you have already read: what you found (file:line), what is still unknown, and the next step. "
                "List every finding with its file:line: this text is all that survives into the next request. Under 250 words, no preamble. "
                "If you were asked for a specific reply format, use it."
            )
        self.session.context_manager.add_user_message(f"{why}. No more tool calls. {ask}")
        async for ev in self._answer_without_tools(task):
            yield ev

    async def _answer_without_tools(self, task: TaskState) -> AsyncGenerator[AgentEvent, None]:
        """One last model call with no tools, answering from what is already in context."""
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

    def _spent(self) -> int:
        """Work done so far: new input plus output. A cached prefix re-sent on every call is not counted,
        or twenty greps on a 20k context would use up the whole budget."""
        u = self.session.context_manager.total_usage
        return u.prompt_tokens - u.cached_tokens + u.completion_tokens

    def _call_limit(self) -> int:
        """Read-only requests (review, explain, find) get a hard cap on tool rounds (model turns that call
        tools; parallel calls in one turn count once): a model that is only reading never has to stop by itself, and 63 calls on a 3-commit review is a cost, not thoroughness.
        Requests that change files and sub-agents are not capped here (they have turn and token budgets)."""
        budget = self.config.read_call_budget if self._read_only else self.config.write_call_budget
        if self.config.model_role != "default" or not budget:
            return 0
        return self._allowance or budget  # after /more: exactly that many more calls

    async def _tidy_if_cache_went_cold(self) -> AsyncGenerator[AgentEvent, None]:
        """The first call after a pause is billed in full because the provider's prompt cache expired.
        Rewriting history costs nothing extra at that moment, so shrink it now: clear old tool output,
        and summarise if the context is still big. Example: back after 10 minutes with 70k of history,
        the cold call sends ~30k instead of 70k."""
        cm = self.session.context_manager
        if self.config.pi_mode or not self._last_model_call or time.monotonic() - self._last_model_call < self.config.cache_ttl_seconds:
            return
        self._last_model_call = 0.0
        before = cm._latest_usage.total_tokens
        self._prune(remind=True, force=True)
        if before > self.config.idle_compact_at_tokens and await self._compact():
            yield AgentEvent.notice(f"Cache had expired: compacted {before // 1000}k of context before the first call")

    FILE_CHARS, FILES_CHARS = 40_000, 150_000  # ~10k tokens a file, ~37k in all: a bit past Claude Code's post-compact budget

    def _restore_files(self, paths: list[str]) -> None:
        """Put the current text of the files the model was just working in back after a compaction.
        Read from disk now, so an edit made before the compaction shows as it is, not as it was first read."""
        parts, room = [], self.FILES_CHARS
        for rel in paths:
            path = resolve_path(self.config.cwd, rel)
            try:
                text = path.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError):
                continue  # deleted, a folder, or binary: the summary still names it
            if room <= 0:
                break
            cap = min(self.FILE_CHARS, room)
            body = text if len(text) <= cap else text[:cap] + f"\n[cut at {cap} of {len(text)} chars; read_file with offset for the rest]"
            room -= len(body)
            parts.append(f"## {rel}\n{body}")
        if parts:
            self.session.context_manager.add_user_message(
                "Files you were working in before the compaction, as they are on disk now (no need to read them again):\n\n"
                + "\n\n".join(parts)
            )

    def _remind_todos(self) -> None:
        """Old results get cleared and history gets folded, but the task list is small and still the plan:
        put it back after a clear so the agent does not rediscover what is left to do."""
        todos = self.session.tool_registry.get("todos")
        text = todos.render() if todos else ""
        if text and "[ ]" in text and text != self._todos_reminded:
            self._todos_reminded = text
            # the list is only as current as the last `todos set`: say so, or a list nobody ticked reads as ten open steps
            self.session.context_manager.add_user_message(
                "Task list as last set (old tool results were cleared). Steps you already finished but did not tick are "
                f"still shown open: tick them with `todos` first, then carry on with the rest.\n{text}"
            )

    def _prune(self, remind: bool = False, force: bool = False) -> None:
        cm = self.session.context_manager
        # Clearing results early only hurts: the model re-reads what it just lost (and any subagent report
        # or AGENTS.md read goes with them), and the rewrite throws away the provider's cache. Claude Code
        # waits until the context is genuinely filling up; so do we (three quarters of the compaction limit), unless
        # the cache has gone cold anyway and the rewrite is free.
        if not force and cm._latest_usage.total_tokens < min(0.75 * cm._limit(), self.config.prune_at_tokens):
            return
        if cm.prune_tool_outputs():
            if remind:
                self._remind_todos()
            # only the files whose text was cleared are forgotten; files still in context are not sent again
            reg = self.session.tool_registry
            reader = reg.get("read_file")
            for rel in cm.last_prune["read_paths"]:
                reader.forget(resolve_path(self.config.cwd, rel))
            # folder rules (apps/web/AGENTS.md) rode in on an old result that may be gone now: attach them again next time
            reg.forget_instructions()
            self.session.tracer.emit("prune", **{**cm.last_prune, "read_paths": len(cm.last_prune["read_paths"])})

    async def _compact(self) -> bool:
        cm = self.session.context_manager
        before = cm._latest_usage.total_tokens
        await self.session.hook_system.fire(HookTrigger.PRE_COMPACT, tokens=before)
        summary, usage, cut = await self.session.chat_compactor.compress(cm)
        if not summary:
            return False
        recent = cm.recent_files(cut)
        self.session.tool_registry.epoch += 1  # earlier reads are no longer in context
        self.session.tool_registry.forget_instructions()  # and neither are the folder rules that came with them
        self._playbooks_sent.clear()  # the summary does not carry playbook text: send it again next time
        cm.apply_compaction(summary, cut, get_data_dir() / "transcripts" / f"{self.session.session_id}.jsonl")
        self._restore_files(recent)
        self._remind_todos()
        cm.set_latest_usage(usage)
        cm.add_usage(usage)
        await self.session.hook_system.fire(HookTrigger.POST_COMPACT, tokens_before=before, summary=summary)
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
                self._no_approver = self._no_approver or bool(result.metadata.get("no_approver"))
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
