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

from agent import review_memory, review_target
from agent.review import _git, build_diff, review_context

MAX_GROUPS = 3
MAX_VERIFIED = 10
SEVERITY_ORDER = {"CRITICAL": 0, "HIGH": 1, "MEDIUM": 2, "LOW": 3}
SKIP_SUFFIXES = (".lock", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", ".woff", ".woff2", ".map", "package-lock.json")
NAMED_SCOPE = re.compile(r"\b(commits?|pr|pull request|HEAD|branch|[0-9a-f]{7,40}|#\d+)\b", re.I)  # a named scope is not "the changes"

# Each reviewer reads the same change through one lens, the way Claude Code runs a bug scan, a rules audit and a
# history check side by side. A single generalist skims; four specialists each go deep on one question.
LENSES = [
    ("behaviour", """LENS: behaviour. For every changed function, write down the claim it now makes ("returns null only when X"), then read its
callers and callees and try to break the claim. For every new or changed condition, flag or gate, find everything that flips it at
runtime and say what the user sees then (a button disabled while isFetching, and every mutation refetches it: it dims on every tap).
Compare the new version of each component, page or function with the old one (`git show HEAD:<file>` or the base revision): list what the
old one offered (actions, states, copy, guards, escape hatches) and say what the user can no longer do. A one-time secret or irreversible
result that a stray click, Escape or backdrop tap can dismiss is a defect. Check wrong operands, inverted conditions, stale state, async ordering, races, leaks, off-by-one, null paths. Read what the diff deleted
before saying a replacement is missing: the new code often leans on a library doing that work."""),
    ("approach", """LENS: approach. Judge the change the way the owner of this system would, not line by line. Is this the right fix at the right layer,
or a patch on the symptom while the cause stays (a guard in one caller when every caller shares the broken function)? Is there a simpler
change that does the same job (delete the branch, reuse the helper that already exists a few files over, change the data instead of the code)?
What is the blast radius: who else calls or deploys this, does old data, an old client or a half-finished rollout still work, can it be
rolled back? Does it follow how the neighbouring code solves the same problem, and if it does not, is the new way better? A real concern
about the design is a finding even when every line is correct: name the better shape and what it saves. Production readiness: a schema or
data-shape change needs a migration or a backfill; a changed API, event or config key must keep old callers and stored data working."""),
    ("failures", """LENS: failures and contracts. For every new awaited call or write on a request path (DB insert, cache write, outbound fetch), ask
what the user gets when it throws; optional data that now fails the whole response is a finding. Look for swallowed errors, empty
catches and fallbacks that hide a real failure. For every changed signature, return shape, event, route, schema, env var, cache key or
migration, grep every caller and consumer and open the line that reads it: the type may say one thing and the runtime data another.
Missing tests count only where the project rules ask for them or a risky branch has none."""),
    ("rules", """LENS: rules and intent. Check the diff against the PROJECT RULES and PLAYBOOKS above: every finding quotes the rule in a few
words and the line that breaks it; a rule that is about how the author works (never commit, ask first) is not a review finding. Then
check the request. A request is a vision, not a full spec: for behaviour it is silent on, a reasonable user's expectation is a requirement
(a delete button that works on every row but the last is a defect even though nobody wrote "the last row"). Is something it asked for missing from the diff, and is something it did not ask for in it (scope creep, a new
dependency, a rewrite of working code)? Also read the comments in the touched files: does the change break guidance written there?"""),
    ("history", """LENS: history. Use git: `git log -5 --oneline -- <file>`, `git blame` or `git log -L` on the changed region, `git show HEAD:<file>`.
Look for a change that undoes an earlier fix (read that commit's message), an invariant a previous commit set up that the diff
breaks, and a sibling the change should have followed (grep for the same pattern next door: if five handlers do X and the new one
does not, say why that is a defect). A defect the old code already had is not this change's."""),
]

SECURITY_LENS = ("security", """LENS: security and isolation. The diff touches auth, data access, secrets or money, so trace who can reach it: is every new
route, handler or query behind the same authorization its siblings use? Is a tenant, owner or user id taken from the request instead
of the session? Injection (SQL, shell, path, HTML) from any value that crosses a trust boundary, secrets or tokens in logs and
responses, fail-open checks (an error path that returns "allowed"), missing rate limits or idempotency on a write that moves money
or sends mail. Read the middleware and the sibling handlers before claiming a gap.""")
GAP_LENS = """LENS: what the others missed. Four reviewers, each with one angle, already reported the findings below. Do not repeat
them. Re-read the riskiest hunk of the diff yourself and ask the question none of the angles owns: what input, ordering or state
did nobody try? If one of their findings is wrong, say so on a line starting with REJECT.
ALREADY REPORTED:
{found}"""
SECURITY_HINT = re.compile(r"\b(auth\w*|token|password|passwd|secret|permission\w*|tenant\w*|session|cookie|jwt|oauth|csrf|cors|payment\w*|stripe|webhook\w*|"
                           r"role|admin|rbac|acl|sanitiz\w*|escape|exec|eval|innerHTML|queryRaw|\$queryRaw|SELECT|INSERT|DELETE)\b", re.I)
BIG_DIFF_LINES = 200
GAP_MIN_LINES = 80

REVIEWER_PROMPT = """You are a principal engineer reviewing {scope}. You did not write it, and you will be accountable if it ships broken.
The request: {request}

FILES: {part}
{moved}OTHER CHANGED FILES (read them only to follow a call chain): {others}

{context}DIFF:
{diff}

{lens}

Hold the bar a principal engineer holds: find the defect that pages someone at 3am, not the ten that make a review look busy. Explain each finding
with the trigger ("when X happens"), the path that reaches it, and what the user or the system does then. Prefer the root cause over the symptom.

How to work: first list, for yourself, every behaviour the diff changes and what could go wrong with each; then check each one by reading
with your tools (callers, callees, config, tests, `git show HEAD:<file>`). Read before you claim; a claim about how a library behaves counts only
if you read the library or its docs. Say what the fix should be in the smallest form: deleting code, bumping a cache key or version, or
removing a branch beats adding a try/catch or a fallback. One line per defect: the same cause hit in two places is one finding naming both.
Do report what the change itself introduces even when it compiles: a feature, action or state the old code offered that the new one silently
lost (a back button, an empty state, a way to recover a one-time value); a branch or route that nothing reaches; the same logic
now written twice; one thing named two ways across the surfaces that show it; a visible restyle the project rules say to confirm first.
Never report: style, redundancy that was already there, anything a linter or typechecker catches, defects on lines the diff did not touch,
defects the old code already had, behaviour that is clearly the intent of the change, pedantic nitpicks a senior engineer would not raise,
missing tests or docs unless the project rules ask for them, a rule the code explicitly silences (a lint-ignore comment), or anything you cleared in your own thinking.

Reply with at most 5 lines, one finding per line, EXACTLY:
SEVERITY | path:line | what is wrong, and who is affected (the call chain you read) | fix
SEVERITY is HIGH only for a defect you proved by reading the code; a guess is MEDIUM and says "unverified".
If you found nothing, reply: NONE
Before the findings put these lines (each on its own line):
SUMMARY: <one or two sentences saying what the change does, in plain words>
STRENGTH: <one specific thing the change does well, with file:line; leave the line out if nothing stands out, never flatter>
ASIDE: <a behaviour you looked at and set aside as out of scope or intended, and why; at most 2 such lines, none if none>"""

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
A claim that cites a project rule counts only if that rule exists in the instruction file and the cited line really breaks it
(open the file: a rule paraphrased from memory is REJECTED). A claim about code the diff did not touch is REJECTED. So is a pedantic nitpick a senior engineer would not raise, a
general quality complaint (missing tests, docs, "poor design") the project rules do not ask for, and a rule the code explicitly silences.
If two claims are the same cause (one missing guard hit from several callers), write SAME_AS <the lower number> on the later one.

Think each claim through before you answer: what input or state triggers it, who gets there, and what they see. Reply with one line per claim, EXACTLY:
N | CONFIRMED or REJECTED or UNCLEAR | path:line what you read that decides it | who is actually affected (n/a if rejected) | ONGOING or TEMPORARY (in under 10 words, e.g. "cache entries before deploy, 15 min") | NEW or ALREADY_AT_HEAD | NONE or SAME_AS <number> | CONF <0-100>
CONF is how sure you are the defect is real AND will be hit in practice: 0 a false positive or pre-existing, 25 maybe, 50 real but rare or minor,
75 verified and will be hit, 100 certain and frequent. A function whose documented contract the change breaks is 75 or more even when this repo
has no other caller: callers are outside the diff, and the next one gets the wrong answer. Judge it as a senior engineer would.
CONFIRMED means you read the code that proves it. REJECTED means the code contradicts it or already handles it (say where).
UNCLEAR means you could not decide with the files available. Nothing else."""


def split_lenses(context: str) -> list[tuple[str, str]]:
    """A big change is cut into areas and every area gets behaviour + approach, plus the rules audit when the repo has rules
    (the UI guard, "ask before restyling" and friends only get checked if some reviewer is told to check them)."""
    return [*LENSES[:2], *([LENSES[3]] if "PROJECT RULES" in context else [])]


def pick_lenses(config, diff: str, files: list[str], context: str) -> list[tuple[str, str]]:
    """How many angles a change deserves. A 10-line fix gets two; a big one gets five; auth, queries and money add a fifth.
    `review_lenses` > 0 pins the count instead (1 = a single generalist)."""
    lines = diff.count("\n")
    if config.review_lenses > 0:
        picked = LENSES[: config.review_lenses]
    elif lines < 40 and len(files) <= 2:
        picked = LENSES[:2]  # behaviour + approach
    elif lines < BIG_DIFF_LINES:
        picked = [*LENSES[:3], LENSES[3] if context else LENSES[4]]
    else:
        picked = list(LENSES)
    if config.review_lenses != 1 and SECURITY_HINT.search(" ".join(files) + "\n" + diff):
        picked = [*picked, SECURITY_LENS]
    return picked


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
    confidence: int = 100  # the verifier's 0-100 belief that this is real and will be hit; below the cutoff it is dropped


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


def frame(answer: str, summary: str, reviewed: list[str], moves: list[tuple[str, str]], deleted: list[str], checks: list | None = None, strengths: list[str] | None = None, aside: list[str] | None = None) -> str:
    """What a human reviewer writes around the findings: what the change does, what was and was not looked at.
    Without it "No issues found." says nothing about how much was read."""
    lines = [f"What the change does: {summary}"] if summary else []
    if strengths:
        lines.append("Done well: " + " ".join(dict.fromkeys(x.strip() for x in strengths))[:400])
    lines.append(answer)
    if aside:
        lines.append("Looked at and set aside: " + "; ".join(dict.fromkeys(x.strip() for x in aside))[:400])
    seen = [f"{len(reviewed)} file(s)"]
    if moves:
        seen.append(f"{len(moves)} moved unchanged (" + ", ".join(f"{a} -> {b}" for a, b in moves[:3]) + (", ..." if len(moves) > 3 else "") + ")")
    if deleted:
        seen.append(f"{len(deleted)} deleted")
    ran = "Ran: " + ", ".join(f"{c.command} ({'passed' if c.ok else 'FAILED'})" for c in checks) + "." if checks else "Not run: typecheck, lint, tests."
    lines.append("Read " + ", ".join(seen) + ". " + ran + (" Stage the move with git add -A on both paths so git keeps the rename." if moves else ""))
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


def parse_verdicts(text: str, count: int) -> dict[int, tuple[str, str, str, str, bool, int, int]]:
    """One `N | VERDICT | evidence | reach | ONGOING/TEMPORARY (how long) | NEW/ALREADY_AT_HEAD | SAME_AS | CONF n` line per claim.
    A claim the verifier skipped stays UNCLEAR; the trailing fields are optional (an older reply without them reads as ongoing, new, sure)."""
    out = {n: ("UNCLEAR", "", "", "", False, 0, 100) for n in range(1, count + 1)}
    for m in re.finditer(r"^\W*(\d+)\s*\|\s*(CONFIRMED|REJECTED|UNCLEAR)\s*\|(.*)$", text or "", re.I | re.M):
        n = int(m.group(1))
        if n not in out:
            continue
        rest = [p.strip() for p in m.group(3).split("|")]
        rest += [""] * (6 - len(rest))
        twin = re.search(r"SAME_AS\s*#?(\d+)", rest[4], re.I)
        conf = re.search(r"(\d{1,3})", rest[5])
        lasts = re.sub(r"^TEMPORARY\b[\s:(-]*", "", rest[2], flags=re.I).rstrip(") ") if rest[2].upper().startswith("TEMPORARY") else ""
        if rest[2].upper().startswith("TEMPORARY") and not lasts:
            lasts = "temporary"
        out[n] = (m.group(2).upper(), rest[0][:400], rest[1][:200], lasts[:120], "ALREADY" in rest[3].upper(), int(twin.group(1)) if twin else 0, min(100, int(conf.group(1))) if conf else 100)
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

    def alike(a: str, b: str) -> bool:
        wa, wb = words(a), words(b)
        return bool(wa and wb) and len(wa & wb) >= 0.6 * min(len(wa), len(wb))

    merged: list[Finding] = []
    for f in findings:
        same = next((m for m in merged if m.where.split(":")[0] == f.where.split(":")[0]
                     and ((m.fix and f.fix and alike(m.fix, f.fix)) or alike(m.what, f.what))), None)
        if same is None:
            merged.append(f)
            continue
        place = f.where.split(":", 1)[1] if ":" in f.where else f.where
        if place not in same.where:  # four angles hitting the same line name it once
            same.where += ", " + place
    return merged


def compose(findings: list[Finding], reviewers: int) -> str:
    """The block the main agent answers from. Rejected findings are listed so it can say what was ruled out, but never as findings."""
    live = sorted((f for f in findings if f.verdict in ("CONFIRMED", "UNCLEAR", "UNCHECKED")), key=lambda f: SEVERITY_ORDER[shown_severity(f)])
    rejected = [f for f in findings if f.verdict == "REJECTED"]
    lines = [f"{reviewers} reviewer(s) read the change in parallel, each through its own angle; each finding was then checked by a separate agent that tried to refute it."]
    if not live and not rejected:
        lines.append("No reviewer found a defect.")
    for f in live:
        tag = "confirmed" if f.verdict == "CONFIRMED" else "unverified"
        lines.append(f"- [{tag}] {shown_severity(f)} {f.where}  {f.what} -> {f.fix}" + (f"  (evidence: {f.evidence})" if f.evidence else "") + (f"  (affects: {f.reach})" if f.reach and f.reach != "n/a" else ""))
    for f in rejected:
        lines.append(f"- [rejected] {f.where}  {f.what[:90]}  because: {f.evidence or 'the code contradicts it'}")
    return "\n".join(lines)


def format_answer(findings: list[Finding]) -> str:
    """The review a user reads, built from the verified findings with no model call: a verdict first, blockers before
    the rest, and for each finding where it is, why it bites and what to do. Example:

        Verdict: changes requested (1 blocker, 1 to fix)
        Blocking
        1. a.ts:4  insert 500s the list
           Affects: every visitor   Evidence: read a.ts:4
           Fix: catch it
    """
    live = sorted((f for f in findings if f.verdict != "REJECTED"), key=lambda f: SEVERITY_ORDER[shown_severity(f)])
    ruled = [f for f in findings if f.verdict == "REJECTED"]
    if not live:
        return "Verdict: approve. No issues found." + (f" The verifier ruled out {len(ruled)} claim(s)." if ruled else "")
    blocking = [f for f in live if shown_severity(f) in ("CRITICAL", "HIGH")]
    rest = [f for f in live if shown_severity(f) == "MEDIUM"]
    minor = [f for f in live if f not in blocking and f not in rest]
    head = "Verdict: changes requested" if blocking else "Verdict: approve with comments"
    counts = ", ".join(x for x in (f"{len(blocking)} blocker(s)" if blocking else "", f"{len(rest)} to fix" if rest else "", f"{len(minor)} minor" if minor else "") if x)
    out = [f"{head} ({counts})"]
    n = 0
    for title, group in (("Blocking", blocking), ("Should fix", rest), ("Minor", minor)):
        if not group:
            continue
        out.append(title)
        for f in group:
            n += 1
            notes = [" (unverified)" if f.verdict != "CONFIRMED" else "", f" (temporary: {f.temporary})" if f.temporary else "",
                     " (the code before this change already did this)" if f.at_head else ""]
            block = [f"{n}. {shown_severity(f)}{''.join(notes)} {f.where}  {f.what}"]
            facts = [x for x in (f"Affects: {f.reach}" if f.reach and f.reach != "n/a" else "", f"Evidence: {f.evidence}" if f.evidence else "") if x]
            if facts:
                block.append("   " + "   ".join(facts))
            if f.fix:
                block.append(f"   Fix: {f.fix}")
            out.append("\n".join(block))
    if ruled:
        shown = "; ".join(f"{f.where} ({(f.evidence or 'the code contradicts it')[:70]})" for f in ruled[:3])
        out.append(f"Checked and ruled out: {len(ruled)} claim(s)" + (f", e.g. {shown}" if shown else ""))
    out.append("Ready to merge? " + ("No: fix the blocker(s) first." if blocking else "With fixes." if rest else "Yes, the minor notes can follow."))
    return "\n\n".join(out)


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
    from agent.review_claude import ask_claude, available
    from config.config import Config

    if config.review_backend == "claude-code" and available():
        answer, tokens = await ask_claude(config, prompt)
        if answer.strip():
            _spent_log.append(tokens)
            return answer
        # an empty answer (not logged in, timeout) falls through to a mew reviewer instead of losing the review

    cfg = Config(**{
        **config.to_dict(),
        "model_role": role,
        "allowed_tools": ["read_file", "grep", "glob", "list_dir", "graph", "search", "outline", "git", "use_skill"],
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


CHECK_WAIT = 90  # seconds the reviewers wait for typecheck/lint before starting without it


async def run_checks(config, files: list[str]):
    """The project's own typecheck / lint / related specs for the changed files, run once and shared with every reviewer."""
    from agent.verify import run_check, select_checks

    try:
        return [await run_check(cmd, Path(config.cwd) / sub, config) for cmd, sub in select_checks(config, files)]
    except Exception:  # a check that cannot start is a missing signal, never a failed review
        return []


def check_findings(results, files: list[str]) -> list[Finding]:
    """A failed check that names a changed file is a proven defect with no model involved: `src/a.ts(12,5): error TS2322`.
    A failure that names none of them (old debt elsewhere) is left for the frame to mention, not blamed on this change."""
    out = []
    for r in results:
        if r.ok:
            continue
        for f in files:
            m = re.search(rf"{re.escape(f)}[(:](\d+)", r.output_tail)
            if m:
                line = next((ln for ln in r.output_tail.splitlines() if f in ln), "").strip()
                out.append(Finding("HIGH", f"{f}:{m.group(1)}", f"`{r.command}` fails here: {line[:160]}", f"make `{r.command}` pass", "CONFIRMED", f"exit {r.exit_code} of {r.command}", "everyone who builds this"))
                break
    return out


async def run_review_pipeline(config, request: str) -> AsyncGenerator[tuple[str, str], None]:
    """Yields ("notice", text) as it goes and finally ("result", block), or nothing when there is nothing to review.
    Reviewers each take one angle on the change (more of them for a bigger or riskier diff), the project's checks run beside
    them, a last reviewer hunts for what they all missed, then ONE verifier tries to refute every claim and scores it."""
    cwd = Path(config.cwd)
    target = await review_target.resolve(cwd, request, SKIP_SUFFIXES)
    if target is None and NAMED_SCOPE.search(request):
        return  # it names a commit/PR/branch we cannot read from here: the normal review answers
    moves: list[tuple[str, str]] = []
    deleted: list[str] = []
    if target:
        files, rev, label = target.files, target.rev, target.label
    else:
        porcelain = await _git(cwd, "status", "--porcelain", "-uall")  # -uall: a new folder is listed file by file, not as one "dir/" entry
        files = changed_files(porcelain)
        moves, deleted, files = await pair_moves(cwd, deleted_files(porcelain), files)
        files = files + deleted  # build_diff shows a deletion as what was removed
        rev, label = ("HEAD",), "the whole uncommitted change"
    if not files and not moves:
        return
    whole = await build_diff(cwd, files, rev)
    groups = [("the whole change", files)] if "[diff clipped]" not in whole else group_files(files)
    yield "notice", f"Reviewing {len(files)} changed file(s)" + (f" in {label}" if target else "") + (f" and {len(moves)} moved unchanged" if moves else "") + (f" with {len(groups)} reviewers in parallel" if len(groups) > 1 else "")
    role = config.review_pipeline_role
    stayed = await left_behind(cwd, moves)
    _spent_log.clear()

    check_task = asyncio.create_task(run_checks(config, files)) if config.review_run_checks and (target is None or target.includes_worktree) else None
    checks: list = []
    if check_task:
        try:
            checks = await asyncio.wait_for(asyncio.shield(check_task), CHECK_WAIT)
        except asyncio.TimeoutError:
            yield "notice", f"Checks are still running after {CHECK_WAIT}s; the reviewers start without them"
        if checks:
            yield "notice", "Checks: " + ", ".join(f"{c.command} {'ok' if c.ok else 'FAILED'}" for c in checks)
    checked = ("AUTOMATED CHECKS ALREADY RUN (a failure naming a changed file is already reported; do not repeat it, do not re-run them): "
               + "; ".join(f"{c.command}={'ok' if c.ok else 'FAILED'}" for c in checks) + "\n\n") if checks else ""
    context = checked + review_memory.render(cwd) + review_context(config, request, files)
    context = context + "\n\n" if context else ""
    lenses = pick_lenses(config, whole, files, context) if len(groups) == 1 else split_lenses(context)  # a split diff already multiplies the reviewers
    if len(lenses) > 1:
        yield "notice", f"Each reviewer takes one angle: {', '.join(n for n, _ in lenses)}" + (" (rules and playbooks loaded)" if "PROJECT RULES" in context or "PLAYBOOKS" in context else "")
    backend = " through Claude Code" if config.review_backend == "claude-code" else ""
    if backend:
        yield "notice", "Reviewers run as Claude Code sessions (read-only)"

    async def review(name: str, fs: list[str], lens: str) -> list[Finding] | None:
        diff = whole if len(groups) == 1 else await build_diff(cwd, fs, rev)
        others = ", ".join(f for f in files if f not in fs)[:600] or "(none)"
        scope = label if len(groups) == 1 else f"one part ({name}) of {label} that {len(groups)} reviewers split"
        if target and not target.includes_worktree:
            scope += f" (a committed range: read old code with `git show {rev[0]}:<file>` and new code with `git show {rev[1]}:<file>`)"
        moved = ("MOVED WITHOUT ANY CHANGE (identical bytes, so there is nothing in them to review; check that imports and paths still resolve): "
                 + "; ".join(f"{a} -> {b}" for a, b in moves)
                 + "\nSTILL IN THE OLD FOLDER (did any of these rely on what moved, e.g. inherit its loading.tsx, layout or index export? say what they lose, or that they are fine): "
                 + (", ".join(stayed) or "(nothing)") + "\n") if moves else ""
        prompt = REVIEWER_PROMPT.format(scope=scope, request=request[:1500], part=", ".join(fs[:25]), moved=moved, others=others, context=context, diff=diff, lens=lens)
        reply = await ask(config, prompt, role, max_turns=12, max_tokens=config.review_pipeline_tokens)
        summaries.extend(re.findall(r"(?im)^\W*SUMMARY:\s*(.+)$", reply))
        strengths.extend(re.findall(r"(?im)^\W*STRENGTH:\s*(.+)$", reply))
        aside.extend(re.findall(r"(?im)^\W*ASIDE:\s*(.+)$", reply))
        return parse_findings(reply) if reply.strip() else None

    summaries: list[str] = []
    strengths: list[str] = []
    aside: list[str] = []
    replies = await asyncio.gather(*(review(n, fs, lens) for n, fs in groups for _, lens in lenses))
    review_tokens = sum(_spent_log)
    if all(r is None for r in replies):
        yield "notice", "The reviewer gave no answer; reviewing without the pipeline"
        return  # an empty answer is a failure, not "no defects": the normal review loop takes over
    found = [f for part in replies if part for f in part]
    found = merge_duplicates(sorted(found, key=lambda f: SEVERITY_ORDER[f.severity]))

    if config.review_gap_pass and len(groups) == 1 and len(lenses) >= 3 and whole.count("\n") >= GAP_MIN_LINES:
        yield "notice", "A last reviewer hunts for what the others missed"
        reported = "\n".join(f"- {f.severity} {f.where} {f.what}" for f in found) or "(nothing)"
        extra_found = await review("the whole change", files, GAP_LENS.format(found=reported)) or []
        found = merge_duplicates(sorted([*found, *extra_found], key=lambda f: SEVERITY_ORDER[f.severity]))
        review_tokens = sum(_spent_log)

    await fix_paths(found, cwd)
    to_check, extra = found[:MAX_VERIFIED], found[MAX_VERIFIED:]  # past the cap a finding stays unchecked and is reported as unverified
    if to_check:
        yield "notice", f"Verifying {len(to_check)} finding(s) in one pass; the verifier tries to refute and score each"
        claims = "\n".join(f"{n}. {f.severity} | {f.where} | {f.what} | fix: {f.fix or '(none)'}" for n, f in enumerate(to_check, 1))
        reply = await ask(config, VERIFIER_PROMPT.format(claims=claims), role, max_turns=10, max_tokens=config.review_pipeline_tokens, disabled_skills=["code-review"])
        for n, (f, v) in enumerate(zip(to_check, parse_verdicts(reply, len(to_check)).values()), 1):
            f.verdict, f.evidence, f.reach, f.temporary, f.at_head, f.same_as, f.confidence = v
            if f.verdict != "REJECTED" and f.confidence < config.review_min_confidence:
                f.verdict, f.evidence = "REJECTED", f"low confidence ({f.confidence}/100): {f.evidence}"
    for f in extra:
        f.verdict = "UNCHECKED"
    found = merge_same_cause(to_check) + extra
    late = check_task.result() if check_task and check_task.done() and not checks and not check_task.cancelled() else checks
    if check_task and not check_task.done():
        check_task.cancel()
    found = check_findings(late, files) + found  # a failing typecheck on a changed file needs no model to confirm
    review_memory.remember(cwd, [(f.where, f.what, f.evidence) for f in found if f.verdict == "REJECTED" and not f.evidence.startswith("low confidence")])
    confirmed = sum(f.verdict == "CONFIRMED" for f in found)
    rejected = sum(f.verdict == "REJECTED" for f in found)
    verify_tokens = sum(_spent_log) - review_tokens
    yield "notice", f"Verified: {confirmed} confirmed, {rejected} rejected, {len(found) - confirmed - rejected} unverified"
    yield "notice", f"Tokens: reviewers {review_tokens:,} + verifier {verify_tokens:,} = {review_tokens + verify_tokens:,}"
    yield "answer", frame(format_answer(found), " ".join(dict.fromkeys(x.strip() for x in summaries[:6]))[:700], files, moves, deleted, late, strengths[:2], aside[:3])
    yield "result", compose(found, len(groups) * len(lenses))
