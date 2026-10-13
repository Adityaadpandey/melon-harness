"""Independent review pass. A separate agent (its own context, read-only tools,
and, when routing says so, a different model) reads the REAL diff and the
original request and hunts for concrete defects. The author never gets to
grade its own work."""
from __future__ import annotations

import asyncio
import re
from dataclasses import dataclass
from pathlib import Path

DIFF_CAP = 40_000
NEW_FILE_CAP = 6_000

REVIEW_PROMPT = """You are an independent code reviewer. You did not write this change.

ORIGINAL REQUEST:
{request}

FILES CHANGED: {files}

AUTOMATED CHECKS: {checks}

THE AUTHOR'S EXPLANATION OF THE CHANGE:
{claims}

DIFF:
{diff}

First test the author's explanation: for each factual claim about why the old code failed ("X collapses", "Y is never called"), read the code and say whether it is true. A fix built on a claim the code contradicts, such as handling something existing code already handles, is a defect. Then review for: requirements the request asked for but the diff misses; logic errors; broken callers or contracts
(read the callers with your tools before claiming a break); security, authorization and tenant-scoping problems;
concurrency/idempotency problems; missing handling of edge cases; violations of the project rules you were given.
Only report problems you can point to in the code. No style nitpicks, no speculation, no generic praise.

Reply in EXACTLY this format:
VERDICT: PASS
or
VERDICT: ISSUES
1. <file:line> <what is wrong and why it matters> | evidence: <what you read that proves it>
2. ...
"""


@dataclass
class ReviewResult:
    verdict: str  # PASS | ISSUES | UNPARSED
    findings: str


async def _git(cwd: Path, *args: str) -> str:
    p = await asyncio.create_subprocess_exec(
        "git", *args, cwd=cwd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL
    )
    out, _ = await p.communicate()
    return out.decode("utf-8", "replace")


async def build_diff(cwd: Path, files: list[str]) -> str:
    """Tracked files via git diff; untracked (new) files shown whole, since git diff omits them."""
    tracked = set((await _git(cwd, "ls-files", "--", *files)).split("\n")) if files else set()
    parts = []
    if tracked & set(files):
        parts.append(await _git(cwd, "diff", "HEAD", "--", *sorted(tracked & set(files))))
    for f in files:
        if f not in tracked and (cwd / f).is_file():
            body = (cwd / f).read_text(encoding="utf-8", errors="replace")
            clip = "\n... [clipped]" if len(body) > NEW_FILE_CAP else ""
            parts.append(f"--- new file: {f}\n{body[:NEW_FILE_CAP]}{clip}")
    diff = "\n".join(parts)
    return diff[:DIFF_CAP] + ("\n... [diff clipped]" if len(diff) > DIFF_CAP else "")


def parse_verdict(text: str) -> ReviewResult:
    ms = list(re.finditer(r"VERDICT:\s*(PASS|ISSUES)\b(.*)", text or "", re.S | re.I))
    m = ms[-1] if ms else None  # last one wins: a model may restate the format before answering
    if not m:
        return ReviewResult("UNPARSED", (text or "")[:500])
    return ReviewResult(m.group(1).upper(), m.group(2).strip())


async def review_changes(config, request: str, files: list[str], checks: list[dict], claims: str = "") -> ReviewResult:
    from agent.agent import Agent
    from agent.events import AgentEventType
    from config.config import Config

    diff = await build_diff(config.cwd, files)
    cfg = Config(**{
        **config.to_dict(),
        "model_role": "review",
        "allowed_tools": ["read_file", "grep", "search", "outline", "git", "glob", "list_dir", "use_skill"],
        "auto_review": False, "auto_verify": False, "verify_commands": [], "verify_rules": [],
        "subagents": [], "max_turns": 8 if diff.count("\n") < 80 else 14, "max_seconds": 300,
        "approval": "never",  # small diffs get 8 turns: a look, not an expedition
    })
    summary = ", ".join(f"{c['command']}={'ok' if c['ok'] else 'FAILED'}" for c in checks) or "none ran"
    prompt = REVIEW_PROMPT.format(request=request[:3000], files=", ".join(files[:30]), checks=summary, claims=(claims or "(none given)")[:2500], diff=diff)
    texts: list[str] = []
    error = ""
    try:
        async with Agent(cfg) as reviewer:
            async for ev in reviewer.run(prompt):
                if ev.type == AgentEventType.TEXT_COMPLETE and ev.data.get("content"):
                    texts.append(ev.data["content"])
                elif ev.type == AgentEventType.AGENT_ERROR:
                    error = ev.data.get("error", "")
    except Exception as e:  # a broken reviewer must never block or fail the task
        return ReviewResult("UNPARSED", f"reviewer crashed: {type(e).__name__}: {e}")
    result = parse_verdict("\n".join(texts))  # the verdict may sit in an earlier turn than the last text
    if result.verdict == "UNPARSED" and error:
        result.findings = f"reviewer stopped: {error[:300]}"
    elif result.verdict == "UNPARSED" and not texts:
        result.findings = "reviewer produced no answer"
    return result
