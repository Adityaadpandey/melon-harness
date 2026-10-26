"""The advisor: a second, stronger model that reads the whole conversation and says what the executor is about to get wrong.

Same shape as Claude Code's advisor tool. It takes no parameters; calling it forwards everything the agent has seen and done, so
the agent never has to summarise (and never leaves out the inconvenient part). Typical moments:

    before editing        "is this the root cause, or a symptom? what else shares the broken function?"
    before saying done    "does the fix cover the trigger in the finding? which edge case is still open?"
    stuck or switching    "the second attempt fails the same way; what am I not seeing?"
"""
from __future__ import annotations

import json
from typing import Callable

from pydantic import BaseModel

from client.response import StreamEventType
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult

ADVISOR_SYSTEM = """You are the advisor: a senior engineer reading over the shoulder of a coding agent. You see its whole transcript (the task,
every tool call, every result). You cannot run anything; you can only think, so think hard about the logic.

Answer in under 250 words, numbered, most important first:
- Is the agent solving the right problem? Name the root cause if it is fixing a symptom (a guard in one caller while every caller shares the broken function).
- What does the planned or finished change miss: other callers, the mirrored half (desktop/mobile, create/update, server/client), empty/null/error paths, ordering and races, old data and old clients?
- If it believes it is done: which concrete input would still break it? What would you run to prove it (a command or a test), not just read?
- If evidence in the transcript contradicts the plan, say so plainly and say which side wins.
No praise, no restating the task, no generic advice. If the plan is sound, say so in one line and name the one thing to double-check."""

PER_MESSAGE = {"user": 5000, "assistant": 3500, "tool": 1500}
TRANSCRIPT_CAP = 60_000


def render_transcript(messages: list[dict], cap: int = TRANSCRIPT_CAP) -> str:
    """The conversation as plain text. The request that started it is always kept; when the rest is too long the oldest middle goes."""
    rows: list[str] = []
    for m in messages:
        role = m.get("role", "")
        if role == "system":
            continue
        text = m.get("content") or ""
        if isinstance(text, list):
            text = " ".join(p.get("text", "") for p in text if isinstance(p, dict))
        limit = PER_MESSAGE.get(role, 1500)
        row = f"[{role}] {text[:limit]}{' ...[cut]' if len(text) > limit else ''}"
        for call in m.get("tool_calls") or []:
            fn = call.get("function", {})
            row += f"\n  -> {fn.get('name')}({json.dumps(fn.get('arguments', ''), default=str)[:400]})"
        rows.append(row)
    out, used = [], 0
    head, rest = rows[:1], rows[1:]
    for row in reversed(rest):
        if used + len(row) > cap:
            out.append("[... earlier steps left out ...]")
            break
        out.append(row)
        used += len(row)
    return "\n\n".join(head + list(reversed(out)))


MIN_FILES, MIN_ROUNDS, MIN_ERRORS = 3, 6, 3


def is_serious(messages: list[dict]) -> bool:
    """Worth a second opinion: the work spans several files after real effort, or keeps failing. A one-file restyle, a vague
    "make it better" or a quick question is not, and the advisor costs a stronger model reading the whole transcript."""
    files, rounds, errors = set(), 0, 0
    for m in messages:
        if m.get("role") == "tool" and str(m.get("content", "")).startswith(("Error", "Not run")):
            errors += 1
        calls = m.get("tool_calls") or []
        rounds += bool(calls)
        for c in calls:
            try:
                args = json.loads(c.get("function", {}).get("arguments") or "{}")
            except ValueError:
                continue
            if isinstance(args, dict) and isinstance(args.get("path"), str):
                files.add(args["path"])
    return errors >= MIN_ERRORS or (len(files) >= MIN_FILES and rounds >= MIN_ROUNDS)


class AdvisorParams(BaseModel):
    pass


class AdvisorTool(Tool):
    name = "advisor"
    description = (
        "Ask a stronger reviewer model for advice. Takes no parameters: your whole conversation is forwarded. Only for serious work: "
        "a change across several files, a bug whose cause you cannot find, or being stuck after repeated failures. Call it before "
        "committing to the approach or when you think you are done. Never for a one-file edit, a restyle, a vague request or a "
        "question; it will refuse."
    )
    kind = ToolKind.READ
    schema = AdvisorParams

    def __init__(self, config, client=None, transcript: Callable[[], list[dict]] | None = None):
        super().__init__(config)
        self.client = client
        self.transcript = transcript
        self.forced = False  # a review-fix run must consult it whatever the size; the agent sets this per request
        self.calls = 0  # advisor calls in this request; the agent resets it per request
        self.last_call_at = -1  # the registry's write count when it was last called, so "consulted after the last edit" can be checked

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        if self.client is None or self.transcript is None:
            return ToolResult.error_result("The advisor is not connected in this run. Carry on without it.")
        if self.calls >= self.config.advisor_max_calls:
            return ToolResult.error_result(f"Advisor limit reached ({self.config.advisor_max_calls} calls this request). Decide with what you have.")
        if not self.forced and not is_serious(self.transcript()):
            return ToolResult.error_result("The advisor is for serious work (several files, or repeated failures). This task is not there yet: decide with what you have.")
        self.calls += 1
        prompt = [{"role": "system", "content": ADVISOR_SYSTEM},
                  {"role": "user", "content": "TRANSCRIPT SO FAR:\n\n" + render_transcript(self.transcript()) + "\n\nGive your advice now."}]
        advice, final, error = "", "", ""
        try:
            async for ev in self.client.chat_completion(prompt, stream=False, role="advisor"):
                if ev.type == StreamEventType.TEXT_DELTA and ev.text_delta:
                    advice += ev.text_delta.content
                elif ev.type == StreamEventType.MESSAGE_COMPLETE and ev.text_delta:
                    final += ev.text_delta.content  # a non-streaming reply arrives whole on the last event
                elif ev.type == StreamEventType.ERROR:
                    error = ev.error or "unknown error"
        except Exception as e:  # an advisor that cannot answer must not stop the work
            error = f"{type(e).__name__}: {e}"
        advice = advice or final
        if not advice.strip():
            return ToolResult.error_result(f"The advisor could not answer ({error[:160] or 'empty reply'}). Carry on without it.")
        return ToolResult.success_result("ADVISOR:\n" + advice.strip())
