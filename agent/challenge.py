"""The challenger: a second model whose only job is to argue against the plan.

Before melon acts on a risky request (a design choice, a refactor, a migration), a read-only
agent on a different model reads the request and the code and answers: what could this
cost, what is being assumed, what is unverified. Its objections are cited or dropped:

    1. [verified] drops the unique index, so duplicate emails become possible | evidence: db/schema.sql:41
    2. [guess] an old client may still send the removed field | evidence: none

[verified] must point at a file that exists (and a line inside it); anything cited wrongly is
removed, never shown. [guess] is allowed but labelled, so a suspicion never reads as a fact."""
from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

CHALLENGER = "deepseek/deepseek-v4-pro"  # a different model family from the author; override with [routing] challenge = [...]

RISKY = re.compile(
    r"\b(should (we|i)|instead of|rather than|approach|design|architect\w*|migrat\w+|refactor|rewrite|redesign|replace|"
    r"remove|delete|drop|deprecate|rename|split|merge|switch to|move (to|from)|i (think|want|plan|suggest|recommend)|"
    r"we (should|could|will)|recommend\w*|best way|is it (ok|safe|fine)|strategy|schema|breaking)\b", re.I)

PROMPT = """You are the challenger. Your job is to argue against the plan below, not to help carry it out.

REQUEST (including any approach or recommendation the user gave):
{request}

Find what the user may be getting wrong. Look for: what could be LOST (data, behaviour, callers, compatibility, time);
what is ASSUMED but not checked; a simpler or safer way; a requirement that is missing. Use your tools to read the
real code before you object. Do not object to style. Do not praise. If the plan is sound, say so.

Tag every objection by how you know it:
  verified  = you read it in this codebase; evidence must be a real file:line
  plausible = follows from what you read, but you did not see the failure; evidence is the file:line it follows from
  guess     = a suspicion you could not check; evidence: none
Never present a guess as verified. Never cite a file you did not open. At most 5 objections, most serious first.

Reply in EXACTLY this format:
OBJECTIONS: NONE
or
OBJECTIONS:
1. [verified|plausible|guess] <what could go wrong or be lost, and why it matters> | evidence: <file:line, or none>
"""


@dataclass
class Objection:
    confidence: str
    text: str
    evidence: str


def is_risky(request: str) -> bool:
    return bool(RISKY.search(request))


def parse_objections(text: str) -> list[Objection]:
    out = []
    for m in re.finditer(r"^\s*\d+\.\s*\[(verified|plausible|guess)\]\s*(.+?)\s*\|\s*evidence:\s*(.*)$", text or "", re.I | re.M):
        out.append(Objection(m.group(1).lower(), m.group(2).strip(), m.group(3).strip()))
    return out


async def keep_grounded(objections: list[Objection], cwd: Path) -> tuple[list[Objection], int]:
    """Cite or drop. Returns the survivors and how many were removed."""
    from agent.grounding import extract_refs, verify_references

    kept, dropped = [], 0
    for o in objections:
        has_ref = bool(extract_refs(o.evidence))
        _, problems = await verify_references(o.evidence, cwd) if has_ref else (0, [])
        if problems or (o.confidence == "verified" and not has_ref):
            dropped += 1  # a made-up file, or a "verified" claim with nothing to check
            continue
        if o.confidence == "plausible" and not has_ref:
            o.confidence = "guess"  # no citation means it is only a suspicion
        kept.append(o)
    return kept, dropped


def render(objections: list[Objection], dropped: int) -> str:
    rows = [f"{i}. [{o.confidence}] {o.text} (evidence: {o.evidence or 'none'})" for i, o in enumerate(objections, 1)]
    if dropped:
        rows.append(f"({dropped} more removed: the file or line they cited does not exist)")
    return "\n".join(rows)


async def challenge_request(config, request: str) -> tuple[list[Objection], int, str]:
    """Returns (grounded objections, dropped count, error). Never raises: a broken challenger must not block the task."""
    from agent.agent import Agent
    from agent.events import AgentEventType
    from config.config import Config

    routing = config.routing if "challenge" in config.routing else {**config.routing, "challenge": [CHALLENGER]}
    cfg = Config(**{
        **config.to_dict(),
        "model_role": "challenge", "routing": routing, "model_pinned": False,
        "allowed_tools": ["read_file", "grep", "search", "outline", "git", "glob", "list_dir"],
        "auto_review": False, "auto_verify": False, "auto_scout": False, "auto_challenge": False,
        "verify_commands": [], "verify_rules": [], "subagents": [], "max_turns": 8, "max_seconds": 180,
        "approval": "never",
    })
    texts: list[str] = []
    error = ""
    try:
        async with Agent(cfg) as challenger:
            async for ev in challenger.run(PROMPT.format(request=request[:4000])):
                if ev.type == AgentEventType.TEXT_COMPLETE and ev.data.get("content"):
                    texts.append(ev.data["content"])
                elif ev.type == AgentEventType.AGENT_ERROR:
                    error = ev.data.get("error", "")
    except Exception as e:
        return [], 0, f"{type(e).__name__}: {e}"
    kept, dropped = await keep_grounded(parse_objections("\n".join(texts)), config.cwd)
    return kept, dropped, error
