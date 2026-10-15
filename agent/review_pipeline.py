"""Review the way Claude Code's reviews work: split the change, review the parts in parallel, then
have a separate agent try to refute every finding before it reaches the user.

    working-tree changes -> up to 3 reviewers (one per area, each with its own diff and context)
                         -> one verifier per finding (reads the code, may reject it)
                         -> a short result block the main agent answers from

A reviewer that guesses is cheap to catch here: its HIGH has to survive a second agent that reads the
code with no stake in the claim. What the verifier rejects never reaches the answer."""
from __future__ import annotations

import asyncio
import re
from dataclasses import dataclass
from pathlib import Path
from typing import AsyncGenerator

from agent.review import _git, build_diff

MAX_GROUPS = 3
MAX_VERIFIED = 6
SEVERITY_ORDER = {"CRITICAL": 0, "HIGH": 1, "MEDIUM": 2, "LOW": 3}
SKIP_SUFFIXES = (".lock", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", ".woff", ".woff2", ".map", "package-lock.json")
NAMED_SCOPE = re.compile(r"\b(commits?|pr|pull request|HEAD|branch|[0-9a-f]{7,40}|#\d+)\b", re.I)  # a named scope is not "the changes"

REVIEWER_PROMPT = """You are one of {n} reviewers, each reading a different part of the same uncommitted change. You did not write it.
The request: {request}

YOUR PART: {part}
OTHER PARTS (not yours; read them only to follow a call chain): {others}

DIFF OF YOUR PART:
{diff}

Hunt for defects the user would hit. Read the callers and callees with your tools before claiming anything. For every new or changed
condition, flag or gate, find what flips it at runtime. For every new awaited call or write on a request path, ask what the user gets
when it throws (optional data that now fails the whole response is a finding). Read what the diff deleted before saying a replacement
is missing. No style, redundancy or "could drift later" items; nothing you cleared in your own thinking.

Reply with at most 5 lines, one finding per line, EXACTLY:
SEVERITY | path:line | what is wrong, and who is affected (the call chain you read) | fix
SEVERITY is HIGH only for a defect you proved by reading the code; a guess is MEDIUM and says "unverified".
If you found nothing, reply: NONE"""

VERIFIER_PROMPT = """Another agent claims this defect exists in the working tree. Your job is to refute it if you can.

CLAIM: {severity} | {where} | {what} | fix: {fix}

Read the cited code, then the call chain that reaches it (callers, the route or handler, config that changes behaviour), and check
whether something already handles it or the behaviour is intended (read the lines the diff deleted too: `git diff` shows them).
A claim about how a library behaves counts only if you read the library or its docs.

Reply with exactly:
VERDICT: CONFIRMED | REJECTED | UNCLEAR
EVIDENCE: path:line what you read that decides it (one or two lines)
REACH: who is actually affected (one line; write "n/a" if rejected)
CONFIRMED means you read the code that proves it. REJECTED means the code contradicts it or already handles it (say where).
UNCLEAR means you could not decide with the files available."""


@dataclass
class Finding:
    severity: str
    where: str
    what: str
    fix: str
    verdict: str = "UNCHECKED"
    evidence: str = ""
    reach: str = ""


def changed_files(porcelain: str) -> list[str]:
    """Paths from `git status --porcelain`, deleted and binary files left out."""
    files = []
    for line in porcelain.splitlines():
        if len(line) < 4 or "D" in line[:2]:
            continue
        path = line[3:].split(" -> ")[-1].strip().strip('"')
        if path and not path.endswith(SKIP_SUFFIXES):
            files.append(path)
    return files


def group_files(files: list[str], limit: int = MAX_GROUPS) -> list[tuple[str, list[str]]]:
    """Group by area (apps/server, apps/web, ...). More areas than reviewers: the extra ones are folded into the smallest group."""
    areas: dict[str, list[str]] = {}
    for f in files:
        parts = f.split("/")
        areas.setdefault("/".join(parts[:2]) if len(parts) > 2 else parts[0], []).append(f)
    groups = sorted(areas.items(), key=lambda kv: -len(kv[1]))
    while len(groups) > limit:
        name, fs = groups.pop()  # smallest
        small = groups[-1]
        groups[-1] = (f"{small[0]} + {name}", small[1] + fs)
    return groups


def parse_findings(text: str) -> list[Finding]:
    out = []
    for line in (text or "").splitlines():
        parts = [p.strip() for p in line.strip().lstrip("-*•0123456789. ").split("|")]
        if len(parts) >= 3 and parts[0].upper() in SEVERITY_ORDER:
            out.append(Finding(parts[0].upper(), parts[1], parts[2], parts[3] if len(parts) > 3 else ""))
    return out


def parse_verdict(text: str) -> tuple[str, str, str]:
    ms = list(re.finditer(r"VERDICT:\s*(CONFIRMED|REJECTED|UNCLEAR)\b", text or "", re.I))
    if not ms:
        return "UNCLEAR", "", ""
    tail = text[ms[-1].end():]
    ev = re.search(r"EVIDENCE:\s*(.+)", tail)
    reach = re.search(r"REACH:\s*(.+)", tail)
    return ms[-1].group(1).upper(), (ev.group(1).strip() if ev else "")[:400], (reach.group(1).strip() if reach else "")[:200]


def compose(findings: list[Finding], reviewers: int) -> str:
    """The block the main agent answers from. Rejected findings are listed so it can say what was ruled out, but never as findings."""
    live = sorted((f for f in findings if f.verdict in ("CONFIRMED", "UNCLEAR", "UNCHECKED")), key=lambda f: SEVERITY_ORDER[f.severity])
    rejected = [f for f in findings if f.verdict == "REJECTED"]
    lines = [f"{reviewers} reviewers read the change in parallel; each finding was then checked by a separate agent that tried to refute it."]
    if not live and not rejected:
        lines.append("No reviewer found a defect.")
    for f in live:
        tag = "confirmed" if f.verdict == "CONFIRMED" else "unverified"
        sev = f.severity if f.verdict == "CONFIRMED" else ("MEDIUM" if f.severity in ("CRITICAL", "HIGH") else f.severity)  # a claim nobody proved is at most MEDIUM
        lines.append(f"- [{tag}] {sev} {f.where}  {f.what} -> {f.fix}" + (f"  (evidence: {f.evidence})" if f.evidence else "") + (f"  (affects: {f.reach})" if f.reach and f.reach != "n/a" else ""))
    for f in rejected:
        lines.append(f"- [rejected] {f.where}  {f.what[:90]}  because: {f.evidence or 'the code contradicts it'}")
    return "\n".join(lines)


async def ask(config, prompt: str, role: str, max_turns: int, max_tokens: int, disabled_skills: list[str] | None = None) -> str:
    """One read-only sub-agent with its own context; returns its last text. A crash or a stop is an empty answer, never an error."""
    from agent.agent import Agent
    from agent.events import AgentEventType
    from config.config import Config

    cfg = Config(**{
        **config.to_dict(),
        "model_role": role,
        "allowed_tools": ["read_file", "grep", "glob", "list_dir", "graph", "search", "outline", "git"],
        "auto_review": False, "auto_verify": False, "verify_commands": [], "verify_rules": [], "subagents": [],
        "enable_parallel_tasks": False, "mcp_servers": {}, "review_pipeline": False,
        "max_turns": max_turns, "max_total_tokens": max_tokens, "max_seconds": 240, "approval": "never",
        "disabled_skills": [*config.disabled_skills, *(disabled_skills or [])],
    })
    texts: list[str] = []
    try:
        async with Agent(cfg) as sub:
            async for ev in sub.run(prompt):
                if ev.type == AgentEventType.TEXT_COMPLETE and ev.data.get("content"):
                    texts.append(ev.data["content"])
    except Exception:  # a broken reviewer must not stop the review
        return ""
    return texts[-1] if texts else ""


async def run_review_pipeline(config, request: str) -> AsyncGenerator[tuple[str, str], None]:
    """Yields ("notice", text) as it goes and finally ("result", block), or nothing when there is nothing to review."""
    cwd = Path(config.cwd)
    files = changed_files(await _git(cwd, "status", "--porcelain"))
    if not files:
        return
    groups = group_files(files)
    yield "notice", f"Review: {len(files)} changed files, {len(groups)} reviewer(s) in parallel"
    slots = asyncio.Semaphore(max(1, config.max_parallel_agents))

    async def review(name: str, fs: list[str]) -> list[Finding]:
        diff = await build_diff(cwd, fs)
        others = ", ".join(f for f in files if f not in fs)[:600] or "(none)"
        prompt = REVIEWER_PROMPT.format(n=len(groups), request=request[:1500], part=f"{name}: {', '.join(fs[:25])}", others=others, diff=diff)
        async with slots:
            return parse_findings(await ask(config, prompt, "review", max_turns=10, max_tokens=config.review_pipeline_tokens))

    found = [f for part in await asyncio.gather(*(review(n, fs) for n, fs in groups)) for f in part]
    found.sort(key=lambda f: SEVERITY_ORDER[f.severity])
    to_check, extra = found[:MAX_VERIFIED], found[MAX_VERIFIED:]  # past the cap a finding stays unchecked and is reported as unverified
    if to_check:
        yield "notice", f"Verifying {len(to_check)} finding(s); a separate agent tries to refute each"

    async def verify(f: Finding) -> None:
        prompt = VERIFIER_PROMPT.format(severity=f.severity, where=f.where, what=f.what, fix=f.fix or "(none)")
        async with slots:
            reply = await ask(config, prompt, "review", max_turns=6, max_tokens=config.review_pipeline_tokens // 2, disabled_skills=["code-review"])
        f.verdict, f.evidence, f.reach = parse_verdict(reply)

    await asyncio.gather(*(verify(f) for f in to_check))
    for f in extra:
        f.verdict = "UNCHECKED"
    kept = sum(f.verdict in ("CONFIRMED", "UNCLEAR", "UNCHECKED") for f in found)
    yield "notice", f"Verified: {sum(f.verdict == 'CONFIRMED' for f in found)} confirmed, {sum(f.verdict == 'REJECTED' for f in found)} rejected, {kept - sum(f.verdict == 'CONFIRMED' for f in found)} unverified"
    yield "result", compose(found, len(groups))
