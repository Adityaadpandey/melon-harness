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

REVIEWER_PROMPT = """You are reviewing {scope}. You did not write it.
The request: {request}

FILES: {part}
{moved}OTHER CHANGED FILES (read them only to follow a call chain): {others}

DIFF:
{diff}

Hunt for defects the user would hit. Read the callers and callees with your tools before claiming anything. For every new or changed
condition, flag or gate, find what flips it at runtime. For every new awaited call or write on a request path, ask what the user gets
when it throws (optional data that now fails the whole response is a finding). Read what the diff deleted before saying a replacement
is missing. Run `git show HEAD:<file>` to see how the old code behaved: if it already failed the same way, the change did not
introduce it. Say what the fix should be in the smallest form: deleting code, bumping a cache key or version, or removing a branch
beats adding a try/catch or a fallback. One line per defect: the same cause hit in two places is one finding naming both lines.
No style, redundancy or "could drift later" items; nothing you cleared in your own thinking.

Reply with at most 5 lines, one finding per line, EXACTLY:
SEVERITY | path:line | what is wrong, and who is affected (the call chain you read) | fix
SEVERITY is HIGH only for a defect you proved by reading the code; a guess is MEDIUM and says "unverified".
If you found nothing, reply: NONE
Put one line before the findings: SUMMARY: <one or two sentences saying what the change does, in plain words>"""

VERIFIER_PROMPT = """Another agent claims these defects exist in the working tree. Your job is to refute each one if you can.

CLAIMS:
{claims}

For each claim read the cited code, then the call chain that reaches it (callers, the route or handler, config that changes behaviour), and
check whether something already handles it or the behaviour is intended (read the lines the diff deleted too: `git diff` shows them).
A claim about how a library behaves counts only if you read the library or its docs. If a claim names a consumer (a component,
controller or caller that reads the data), open that file and find the exact line that reads it; if that line does not read what the
claim says, REJECT it: a wrong consumer is the usual way a plausible claim is false.

Also settle how bad it is. Is the failing state permanent, or does it go away by itself (entries that expire, a one-time migration,
a window after deploy)? Did the code at HEAD (`git show HEAD:<file>`) already fail the same way, so the change did not add it?
Say ALREADY_AT_HEAD too when the request already depends on the same thing and fails without the change whenever this fails
(a new write to the same Postgres the request already reads: if Postgres is down it 500s anyway; only "reads work, the write fails" is new).
If two claims are the same cause (one missing guard hit from several callers), write SAME_AS <the lower number> on the later one.

Reply with one line per claim, EXACTLY:
N | CONFIRMED or REJECTED or UNCLEAR | path:line what you read that decides it | who is actually affected (n/a if rejected) | ONGOING or TEMPORARY (in under 10 words, e.g. "cache entries before deploy, 15 min") | NEW or ALREADY_AT_HEAD | NONE or SAME_AS <number>
CONFIRMED means you read the code that proves it. REJECTED means the code contradicts it or already handles it (say where).
UNCLEAR means you could not decide with the files available. Nothing else."""


@dataclass
class Finding:
    severity: str
    where: str
    what: str
    fix: str
    verdict: str = "UNCHECKED"
    evidence: str = ""
    reach: str = ""
    temporary: str = ""  # how long a self-healing failing state lasts, empty when it stays
    at_head: bool = False  # the code before the change already failed the same way
    same_as: int = 0  # claim number this one repeats (one cause, several callers); 0 when it stands alone


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


def deleted_files(porcelain: str) -> list[str]:
    """Paths `git status --porcelain` reports as deleted (a file moved away shows up here too)."""
    out = []
    for line in porcelain.splitlines():
        if len(line) >= 4 and "D" in line[:2]:
            out.append(line[3:].strip().strip('"'))
    return out


async def pair_moves(cwd: Path, deleted: list[str], added: list[str]) -> tuple[list[tuple[str, str]], list[str], list[str]]:
    """A file deleted in one place and added somewhere else with identical bytes is a move, not a change.
    Example: events/page.tsx -> events/(list)/page.tsx. Returns (moves, deleted left over, added left over), so the
    reviewer reads the moves as one line instead of a deletion plus a whole new file."""
    gone = {}
    for d in deleted:
        sha = (await _git(cwd, "rev-parse", f"HEAD:{d}")).strip()
        if len(sha) == 40:
            gone.setdefault(sha, []).append(d)
    moves, used_old, moved_new = [], set(), set()
    for a in added:
        if not (cwd / a).is_file():
            continue
        sha = (await _git(cwd, "hash-object", "--", a)).strip()
        if gone.get(sha):
            old = gone[sha].pop(0)
            moves.append((old, a))
            used_old.add(old)
            moved_new.add(a)
    return moves, [d for d in deleted if d not in used_old], [a for a in added if a not in moved_new]


async def left_behind(cwd: Path, moves: list[tuple[str, str]]) -> list[str]:
    """Files still sitting where the moved ones used to be. They may have been leaning on what moved: a route that
    inherited its sibling's loading.tsx or layout, a module imported through an index file. Example: moving
    events/loading.tsx leaves events/[id]/page.tsx without a loading state."""
    out: list[str] = []
    for old, _ in moves:
        folder = old.rsplit("/", 1)[0] if "/" in old else "."
        for f in (await _git(cwd, "ls-files", "--", folder)).split("\n"):
            if f and f not in out and f not in {o for o, _ in moves} and (cwd / f).exists():
                out.append(f)
    return out[:15]


def frame(answer: str, summary: str, reviewed: list[str], moves: list[tuple[str, str]], deleted: list[str]) -> str:
    """What a human reviewer writes around the findings: what the change does, what was and was not looked at.
    Without it "No issues found." says nothing about how much was read."""
    lines = [f"What the change does: {summary}"] if summary else []
    lines.append(answer)
    seen = [f"{len(reviewed)} file(s) read in the diff"]
    if moves:
        seen.append(f"{len(moves)} moved unchanged (" + ", ".join(f"{a} -> {b}" for a, b in moves[:3]) + (", ..." if len(moves) > 3 else "") + ")")
    if deleted:
        seen.append(f"{len(deleted)} deleted")
    lines.append("Looked at: " + "; ".join(seen) + ".")
    if moves:
        lines.append("Next: stage the deletions and the new files together (git add -A on both paths) so git records a rename and keeps the history.")
    lines.append("Not run: typecheck, lint and tests (the reviewers only read code); run them before merging.")
    return "\n\n".join(lines)


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


def parse_verdicts(text: str, count: int) -> dict[int, tuple[str, str, str, str, bool, int]]:
    """One `N | VERDICT | evidence | reach | ONGOING/TEMPORARY (how long) | NEW/ALREADY_AT_HEAD` line per claim.
    A claim the verifier skipped stays UNCLEAR; the last two fields are optional (an older reply without them reads as ongoing and new)."""
    out = {n: ("UNCLEAR", "", "", "", False, 0) for n in range(1, count + 1)}
    for m in re.finditer(r"^\W*(\d+)\s*\|\s*(CONFIRMED|REJECTED|UNCLEAR)\s*\|(.*)$", text or "", re.I | re.M):
        n = int(m.group(1))
        if n not in out:
            continue
        rest = [p.strip() for p in m.group(3).split("|")]
        rest += [""] * (5 - len(rest))
        twin = re.search(r"SAME_AS\s*#?(\d+)", rest[4], re.I)
        lasts = re.sub(r"^TEMPORARY\b[\s:(-]*", "", rest[2], flags=re.I).rstrip(") ") if rest[2].upper().startswith("TEMPORARY") else ""
        if rest[2].upper().startswith("TEMPORARY") and not lasts:
            lasts = "temporary"
        out[n] = (m.group(2).upper(), rest[0][:400], rest[1][:200], lasts[:120], "ALREADY" in rest[3].upper(), int(twin.group(1)) if twin else 0)
    return out


def shown_severity(f: Finding) -> str:
    """What the user sees. HIGH has to be proven AND lasting AND new: a claim nobody proved, a state that heals itself
    (stale cache entries that expire in 15 min) or one the old code already had is at most MEDIUM."""
    if f.severity in ("CRITICAL", "HIGH") and (f.verdict != "CONFIRMED" or f.temporary or f.at_head):
        return "MEDIUM"
    return f.severity


def merge_same_cause(checked: list[Finding]) -> list[Finding]:
    """The verifier saw every claim at once: a claim it marked `SAME_AS 1` is folded into claim 1 (even across files),
    so one missing guard hit from a dashboard and three event pages is one finding naming every place."""
    keep: list[Finding] = []
    for n, f in enumerate(checked, 1):
        twin = checked[f.same_as - 1] if 0 < f.same_as < n else None
        if twin is None or twin.verdict == "REJECTED" or f.verdict == "REJECTED":
            keep.append(f)
            continue
        twin.where += ", " + f.where
        twin.at_head = twin.at_head and f.at_head
        twin.temporary = twin.temporary if f.temporary else ""  # one lasting place makes the whole thing lasting
    return keep


def merge_duplicates(findings: list[Finding]) -> list[Finding]:
    """The same cause reported twice (one fix, same file) becomes one finding naming every place: two lines in
    event-read.service.ts that both need `catch the short-link write` are one defect, not two HIGHs."""
    def words(t: str) -> set[str]:
        return set(re.findall(r"[a-z]{4,}", t.lower()))

    merged: list[Finding] = []
    for f in findings:
        same = next((m for m in merged if m.where.split(":")[0] == f.where.split(":")[0] and m.fix and f.fix
                     and len(words(m.fix) & words(f.fix)) >= 0.6 * min(len(words(m.fix)), len(words(f.fix)))), None)
        if same is None:
            merged.append(f)
        else:
            same.where += ", " + (f.where.split(":", 1)[1] if ":" in f.where else f.where)
    return merged


def compose(findings: list[Finding], reviewers: int) -> str:
    """The block the main agent answers from. Rejected findings are listed so it can say what was ruled out, but never as findings."""
    live = sorted((f for f in findings if f.verdict in ("CONFIRMED", "UNCLEAR", "UNCHECKED")), key=lambda f: SEVERITY_ORDER[shown_severity(f)])
    rejected = [f for f in findings if f.verdict == "REJECTED"]
    lines = [f"{reviewers} reviewers read the change in parallel; each finding was then checked by a separate agent that tried to refute it."]
    if not live and not rejected:
        lines.append("No reviewer found a defect.")
    for f in live:
        tag = "confirmed" if f.verdict == "CONFIRMED" else "unverified"
        lines.append(f"- [{tag}] {shown_severity(f)} {f.where}  {f.what} -> {f.fix}" + (f"  (evidence: {f.evidence})" if f.evidence else "") + (f"  (affects: {f.reach})" if f.reach and f.reach != "n/a" else ""))
    for f in rejected:
        lines.append(f"- [rejected] {f.where}  {f.what[:90]}  because: {f.evidence or 'the code contradicts it'}")
    return "\n".join(lines)


def format_answer(findings: list[Finding]) -> str:
    """The review a user reads, built from the verified findings with no model call: one line each, rejected ones dropped."""
    live = sorted((f for f in findings if f.verdict != "REJECTED"), key=lambda f: SEVERITY_ORDER[shown_severity(f)])
    lines = []
    for f in live:
        proven = f.verdict == "CONFIRMED"
        note = "" if proven else " (unverified)"
        note += f" (temporary: {f.temporary})" if f.temporary else ""
        note += " (the code before this change already did this)" if f.at_head else ""
        lines.append(f"{shown_severity(f)}{note} {f.where}  {f.what}" + (f" -> {f.fix}" if f.fix else ""))
    ruled_out = sum(f.verdict == "REJECTED" for f in findings)
    if not lines:
        return "No issues found." + (f" The verifier ruled out {ruled_out} claim(s)." if ruled_out else "")
    return "\n\n".join(lines) + (f"\n\n{ruled_out} more claim(s) were ruled out by the verifier." if ruled_out else "")


_spent_log: list[int] = []  # new input + output tokens of each sub-agent, in the order they finished


async def fix_paths(findings: list[Finding], cwd: Path) -> None:
    """A path the reviewer wrote must exist. A near miss (`services/event-discovery.service.ts` for `services/event/event-discovery.service.ts`)
    is mended when exactly one file has that name; anything else is marked so the reader does not go looking for it."""
    from agent.grounding import _resolve, _tree

    tree = await _tree(cwd)
    for f in findings:
        path, sep, rest = f.where.partition(":")
        if not path.strip() or _resolve(cwd, path.strip(), tree):
            continue
        same_name = [t for t in tree if t.rsplit("/", 1)[-1] == path.strip().rsplit("/", 1)[-1]]
        f.where = (same_name[0] + sep + rest) if len(same_name) == 1 else f.where + " (file not found)"


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
        "enable_parallel_tasks": False, "mcp_servers": {}, "review_pipeline": False, "auto_challenge": False, "auto_scout": False,  # a challenger per sub-agent cost 80k tokens and 4 minutes
        "max_turns": max_turns, "max_total_tokens": max_tokens, "max_seconds": 150, "approval": "never",
        "disabled_skills": [*config.disabled_skills, *(disabled_skills or [])],
    })
    texts: list[str] = []
    try:
        async with Agent(cfg) as sub:
            async for ev in sub.run(prompt):
                if ev.type == AgentEventType.TEXT_COMPLETE and ev.data.get("content"):
                    texts.append(ev.data["content"])
            _spent_log.append(sub._spent())
    except Exception:  # a broken reviewer must not stop the review
        return ""
    return texts[-1] if texts else ""


async def run_review_pipeline(config, request: str) -> AsyncGenerator[tuple[str, str], None]:
    """Yields ("notice", text) as it goes and finally ("result", block), or nothing when there is nothing to review.
    One reviewer reads the whole change (two when the diff is too big for one reading), then ONE verifier checks every claim."""
    cwd = Path(config.cwd)
    porcelain = await _git(cwd, "status", "--porcelain", "-uall")  # -uall: a new folder is listed file by file, not as one "dir/" entry
    files = changed_files(porcelain)
    moves, deleted, files = await pair_moves(cwd, deleted_files(porcelain), files)
    files = files + deleted  # build_diff shows a deletion as what was removed
    if not files and not moves:
        return
    whole = await build_diff(cwd, files)
    groups = [("the whole change", files)] if "[diff clipped]" not in whole else group_files(files, 2)
    yield "notice", f"Reviewing {len(files)} changed file(s)" + (f" and {len(moves)} moved unchanged" if moves else "") + (f" with {len(groups)} reviewers in parallel" if len(groups) > 1 else "")
    role = config.review_pipeline_role
    stayed = await left_behind(cwd, moves)
    _spent_log.clear()

    async def review(name: str, fs: list[str]) -> list[Finding] | None:
        diff = whole if len(groups) == 1 else await build_diff(cwd, fs)
        others = ", ".join(f for f in files if f not in fs)[:600] or "(none)"
        scope = "the whole uncommitted change" if len(groups) == 1 else f"one part ({name}) of an uncommitted change that {len(groups)} reviewers split"
        moved = ("MOVED WITHOUT ANY CHANGE (identical bytes, so there is nothing in them to review; check that imports and paths still resolve): "
                 + "; ".join(f"{a} -> {b}" for a, b in moves)
                 + "\nSTILL IN THE OLD FOLDER (did any of these rely on what moved, e.g. inherit its loading.tsx, layout or index export? say what they lose, or that they are fine): "
                 + (", ".join(stayed) or "(nothing)") + "\n") if moves else ""
        prompt = REVIEWER_PROMPT.format(scope=scope, request=request[:1500], part=", ".join(fs[:25]), moved=moved, others=others, diff=diff)
        reply = await ask(config, prompt, role, max_turns=8, max_tokens=config.review_pipeline_tokens)
        summaries.extend(re.findall(r"(?im)^\W*SUMMARY:\s*(.+)$", reply))
        return parse_findings(reply) if reply.strip() else None

    summaries: list[str] = []
    replies = await asyncio.gather(*(review(n, fs) for n, fs in groups))
    review_tokens = sum(_spent_log)
    if all(r is None for r in replies):
        yield "notice", "The reviewer gave no answer; reviewing without the pipeline"
        return  # an empty answer is a failure, not "no defects": the normal review loop takes over
    found = [f for part in replies if part for f in part]
    found = merge_duplicates(sorted(found, key=lambda f: SEVERITY_ORDER[f.severity]))
    await fix_paths(found, cwd)
    to_check, extra = found[:MAX_VERIFIED], found[MAX_VERIFIED:]  # past the cap a finding stays unchecked and is reported as unverified
    if to_check:
        yield "notice", f"Verifying {len(to_check)} finding(s) in one pass; the verifier tries to refute each"
        claims = "\n".join(f"{n}. {f.severity} | {f.where} | {f.what} | fix: {f.fix or '(none)'}" for n, f in enumerate(to_check, 1))
        reply = await ask(config, VERIFIER_PROMPT.format(claims=claims), role, max_turns=6, max_tokens=config.review_pipeline_tokens, disabled_skills=["code-review"])
        for n, (f, v) in enumerate(zip(to_check, parse_verdicts(reply, len(to_check)).values()), 1):
            f.verdict, f.evidence, f.reach, f.temporary, f.at_head, f.same_as = v
    for f in extra:
        f.verdict = "UNCHECKED"
    found = merge_same_cause(to_check) + extra
    confirmed = sum(f.verdict == "CONFIRMED" for f in found)
    rejected = sum(f.verdict == "REJECTED" for f in found)
    verify_tokens = sum(_spent_log) - review_tokens
    yield "notice", f"Verified: {confirmed} confirmed, {rejected} rejected, {len(found) - confirmed - rejected} unverified"
    yield "notice", f"Tokens: reviewer {review_tokens:,} + verifier {verify_tokens:,} = {review_tokens + verify_tokens:,}"
    yield "answer", frame(format_answer(found), summaries[0].strip() if summaries else "", files, moves, deleted)
    yield "result", compose(found, len(groups))
