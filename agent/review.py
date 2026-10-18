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

FILES THAT DEPEND ON THE CHANGED CODE (from the code graph; read their call sites before claiming a break):
{impact}

{context}{prior}{fixing}DIFF:
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


COVERED_BY_LENSES = {"code-review", "review-changes", "review-pr", "review-commits"}  # the reviewer prompts already carry their method


def review_context(config, request: str, files: list[str]) -> str:
    """What a human reviewer would have open next to the diff: the AGENTS.md / CLAUDE.md of every folder the change
    touches (root first, @imports inlined) and the playbooks this request calls for (typescript, security, ...).
    Example: a change under apps/web/ brings apps/web/AGENTS.md and the typescript playbook into the reviewer's prompt,
    so "use pnpm" or "never log tokens" is checked instead of hoped for. Empty string when there is nothing."""
    from agent import instructions
    from agent.skills import load_skills, match_skills

    root = Path(config.cwd)
    seen: set[Path] = set()
    rules = []
    for f in files[:40]:
        text = instructions.for_path(root, root / f, seen)
        if text:
            rules.append(text.split("\n\n", 1)[-1])  # drop the "shown once" banner, keep the files
    out = []
    if rules:
        out.append("PROJECT RULES (from the repo's instruction files; a change that breaks one is a finding, quote the rule):\n" + "\n\n".join(rules)[:6000])
    try:
        picked = [s for s in match_skills(load_skills(config), request + " " + " ".join(files[:30]), limit=8) if s.name not in COVERED_BY_LENSES][:3]
    except Exception:  # playbooks only add context; a broken skill file must not stop the review
        picked = []
    if picked:
        out.append("PLAYBOOKS THAT APPLY (the project's own checklists):\n" + "\n\n".join(f"### {s.name}\n{s.body}" for s in picked)[:5000])
    return "\n\n".join(out)


@dataclass
class ReviewResult:
    verdict: str  # PASS | ISSUES | UNPARSED
    findings: str
    blocked: int = 0  # reviewer tool calls a hook or rule refused: its checks were narrower than intended


async def _git(cwd: Path, *args: str) -> str:
    p = await asyncio.create_subprocess_exec(
        "git", *args, cwd=cwd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL
    )
    out, _ = await p.communicate()
    return out.decode("utf-8", "replace")


async def build_diff(cwd: Path, files: list[str], rev: tuple[str, ...] = ("HEAD",)) -> str:
    """Tracked files via git diff against `rev`; untracked (new) files shown whole, since git diff omits them.
    A committed range (two revs, e.g. HEAD~3 HEAD) is just that range: the working tree is not part of it."""
    working = len(rev) == 1
    tracked = set((await _git(cwd, "ls-files", "--", *files)).split("\n")) if files else set()
    parts = []
    gone = {f for f in files if not (cwd / f).exists()}  # deleted or moved away: git diff shows what was removed
    pick = (tracked & set(files)) | gone if working else set(files)
    if pick:
        parts.append(await _git(cwd, "diff", *rev, "--", *sorted(pick)))
    for f in files if working else []:
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


SECOND_LOOK = """A first reviewer already reported this (it may be wrong or incomplete):
{prior}

Do not repeat it. Hunt for defects it MISSED, and say so if one of its findings is false.

"""


FIX_REVIEW = """THIS IS A REVIEW OF A FIX. The author was told about these problems and changed the code to address them:
{fixing}

Judge the fix, not the original idea: is each problem really resolved? Then look for what the fix itself broke: other branches of the same function (a changed return shape), every consumer of a changed output (types, tests, API collections), loading/error/empty states that the new condition no longer covers, and whether a restriction (e.g. hiding data) can still be bypassed another way (e.g. searching by the hidden field).

"""


async def review_changes(config, request: str, files: list[str], checks: list[dict], claims: str = "", prior: str = "", role: str = "review", fixing: str = "") -> ReviewResult:
    from agent.agent import Agent
    from agent.events import AgentEventType
    from config.config import Config

    diff = await build_diff(config.cwd, files)
    cfg = Config(**{
        **config.to_dict(),
        "model_role": role,
        "allowed_tools": ["read_file", "grep", "search", "outline", "git", "glob", "list_dir", "use_skill"],
        "auto_review": False, "auto_verify": False, "verify_commands": [], "verify_rules": [],
        "subagents": [], "max_turns": 8 if diff.count("\n") < 80 else 14, "max_seconds": 300,
        "approval": "never",  # small diffs get 8 turns: a look, not an expedition
    })
    impact = "(no code graph)"
    try:
        from utils import codegraph

        g = codegraph.load(config.cwd)
        if g:
            rows = g.impact(files)
            impact = "\n".join(f"- {f} ({', '.join(labels)})" for f, labels in rows) or "(none found)"
    except Exception:  # the graph only adds hints; a bad one must not stop the review
        pass
    summary = ", ".join(f"{c['command']}={'ok' if c['ok'] else 'FAILED'}" for c in checks) or "none ran"
    context = review_context(config, request, files)
    context = context + "\n\n" if context else ""
    prompt = REVIEW_PROMPT.format(request=request[:3000], files=", ".join(files[:30]), checks=summary, claims=(claims or "(none given)")[:2500], prior=SECOND_LOOK.format(prior=prior[:3000]) if prior else "", fixing=FIX_REVIEW.format(fixing=fixing[:3000]) if fixing else "", diff=diff, impact=impact, context=context)
    texts: list[str] = []
    error = ""
    blocked = 0
    try:
        async with Agent(cfg) as reviewer:
            async for ev in reviewer.run(prompt):
                if ev.type == AgentEventType.TEXT_COMPLETE and ev.data.get("content"):
                    texts.append(ev.data["content"])
                elif ev.type == AgentEventType.AGENT_ERROR:
                    error = ev.data.get("error", "")
                elif ev.type == AgentEventType.TOOL_CALL_COMPLETE and (ev.data.get("metadata") or {}).get("blocked"):
                    blocked += 1
    except Exception as e:  # a broken reviewer must never block or fail the task
        return ReviewResult("UNPARSED", f"reviewer crashed: {type(e).__name__}: {e}")
    result = parse_verdict("\n".join(texts))  # the verdict may sit in an earlier turn than the last text
    result.blocked = blocked
    if result.verdict == "UNPARSED" and error:
        result.findings = f"reviewer stopped: {error[:300]}"
    elif result.verdict == "UNPARSED" and not texts:
        result.findings = "reviewer produced no answer"
    return result


def merge_reviews(first: ReviewResult, second: ReviewResult) -> ReviewResult:
    """Either reviewer finding something makes it ISSUES; a second reviewer that crashed changes nothing."""
    if second.verdict != "ISSUES":
        return first
    if first.verdict != "ISSUES":
        return second
    return ReviewResult("ISSUES", f"{first.findings}\n\nSecond reviewer, additionally:\n{second.findings}")
