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
SPLIT_FILES = 30  # a clipped diff with this many files or fewer stays with one reviewer, who reads the rest with `git diff`
MAX_VERIFIED = 24  # claims checked per review; past it a finding stays unchecked and is reported as unverified
VERIFY_BATCH = 6  # claims one verifier reads at a time, so each gets real turns instead of a share of one budget
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
before saying a replacement is missing: the new code often leans on a library doing that work.
Removed behaviour: for every line the diff deletes or replaces, name the invariant it enforced (a guard, an error path, a validation, a
cleanup, an early exit), then find where the new code establishes it again. If you cannot find it, that is a finding. Also ask what a
line that moved into a new branch does now when the branch is not taken."""),
    ("approach", """LENS: approach. Judge the change the way the owner of this system would, not line by line. Is this the right fix at the right layer,
or a patch on the symptom while the cause stays (a guard in one caller when every caller shares the broken function)? Is there a simpler
change that does the same job (delete the branch, reuse the helper that already exists a few files over, change the data instead of the code)?
Layering: does it use the layer the repo's rules and neighbouring files use (service -> repository -> database), or reach past it?
Duplication the diff creates: the same rule written in two or three places (cache hit, cache miss, public endpoint) belongs in one helper.
Mirrors: when the change touches one of a pair (desktop and mobile layout, create and update, cached and uncached path, server and
client), open the other half and say whether it needs the same change. What is the blast radius: who else calls or deploys this, does old data, an old client or a half-finished rollout still work, can it be
rolled back? Does it follow how the neighbouring code solves the same problem, and if it does not, is the new way better? A real concern
about the design is a finding even when every line is correct: name the better shape and what it saves. Production readiness: a schema or
data-shape change needs a migration or a backfill; a changed API, event or config key must keep old callers and stored data working."""),
    ("failures", """LENS: failures and contracts. For every new awaited call or write on a request path (DB insert, cache write, outbound fetch), ask
what the user gets when it throws; optional data that now fails the whole response is a finding. Look for swallowed errors, empty
catches and fallbacks that hide a real failure. For every changed signature, return shape, event, route, schema, env var, cache key or
migration, grep every caller and consumer and open the line that reads it: the type may say one thing and the runtime data another.
Missing tests: code that guards privacy, auth, money or a permission needs a test, so none in the diff is a finding (the repo's
rules or playbooks usually say so: quote them; check whether a spec file exists on the branch but is missing from the diff).
Performance: a new query, await or loop on a per-request path is a finding when it repeats per item or per visit (an extra DB call
on every cache hit, an N+1); say how often it runs. Accessibility: UI hidden, locked or disabled by CSS alone (pointer-events: none,
opacity) stays focusable and is still read aloud; it needs the attribute or the element removed."""),
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

CONTRACT_LENS = ("contracts", """LENS: producers and consumers. Nothing else is your job. List every value, shape, string format, prop, event, route,
cache key or return type the diff creates or changes. For each, find BOTH sides: the code that produces it and the code that reads it
(grep the name, open every hit). Then compare them line against line, with a real example value pushed through each side:
- A consumer that parses, matches or normalises something the producer already shapes (a regex that wants "3 - Name" when the
  producer writes "3x Name"; a trim or a null check the other side already did) never fires: that is dead code, name both lines.
- A producer that now emits a new shape, null, empty list, enum member or error that a consumer, type, test or API collection still
  assumes away; a field renamed on one side only; a unit, timezone or casing change.
- Both halves of a pair (desktop/mobile, create/update, server/client, cached/uncached): one changed and the twin did not.
- A new fetch, query or lookup whose answer the caller already holds in a payload it received: say which payload and which field.
Quote the producer line and the consumer line in the finding (path:line each) and the example value that breaks it. Report only
mismatches you proved by reading both sides.""")
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
LIGHT_NOTE = """

QUICK PASS: this change is small and the whole diff is above. Do not explore. Read the changed region's file once if you need the lines
around it (and the script header or config it runs under), at most 3 reads in all, then answer. Cover: a failure the new code can cause
(a command that can exit non-zero under the shell options in force, an await that can throw), anything it deletes or widens (a flag that
removes more than before), a second copy of something the file already does, and a project rule the edit breaks (quote it).
Where you did not check something, say so in the finding ("unverified: the script header was not read")."""
LIGHT_TURNS = 12  # a small change has little to read; a reviewer that needs more than this is wandering
SMALL_DIFF_LINES = 80  # one file this small gets one reviewer: a second one re-reads the same few lines

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
removing a branch beats adding a try/catch or a fallback. One line per defect, under 45 words, in plain words (no section numbers, no quoting whole paragraphs): the same cause hit in two places is one finding naming both.
Do report what the change itself introduces even when it compiles: a feature, action or state the old code offered that the new one silently
lost (a back button, an empty state, a way to recover a one-time value); a branch or route that nothing reaches; the same logic
now written twice; one thing named two ways across the surfaces that show it; a visible restyle the project rules say to confirm first.
Never report: style, redundancy that was already there, anything a linter or typechecker catches, defects on lines the diff did not touch,
defects the old code already had, behaviour that is clearly the intent of the change, pedantic nitpicks a senior engineer would not raise,
missing tests or docs unless the project rules ask for them, a rule the code explicitly silences (a lint-ignore comment), or anything you cleared in your own thinking.

Reply with at most 10 lines, one finding per line, EXACTLY:
SEVERITY | path:line | what is wrong, and who is affected (the call chain you read) | fix
SEVERITY is HIGH only for a defect you proved by reading the code; a guess is MEDIUM and says "unverified".
If you found nothing, reply: NONE
Before the findings put these lines (each on its own line):
SUMMARY: <one or two sentences saying what the change does, in plain words>
STRENGTH: <one specific thing the change does well, with file:line; leave the line out if nothing stands out, never flatter>
ASIDE: <a behaviour you looked at and set aside as out of scope or intended, and why; at most 2 such lines, none if none>"""

VERIFIER_PROMPT = """Another agent claims these defects exist in the working tree. Decide each one. Your default is PLAUSIBLE: refuting a real defect costs more than keeping a doubtful one a reader can discard.

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
N | CONFIRMED or PLAUSIBLE or REJECTED or UNCLEAR | path:line what you read that decides it | who is actually affected (n/a if rejected) | ONGOING or TEMPORARY (in under 10 words, e.g. "cache entries before deploy, 15 min") | NEW or ALREADY_AT_HEAD | NONE or SAME_AS <number> | CONF <0-100>
CONF is how sure you are the defect is real AND will be hit in practice: 0 a false positive or pre-existing, 25 maybe, 50 real but rare or minor,
75 verified and will be hit, 100 certain and frequent. A function whose documented contract the change breaks is 75 or more even when this repo
has no other caller: callers are outside the diff, and the next one gets the wrong answer. Judge it as a senior engineer would.
CONFIRMED means you can name the input or state that triggers it and the wrong result, and you read the code that proves it.
PLAUSIBLE means the mechanism is real but the trigger is uncertain (timing, environment, config, a rare but reachable path); say what would confirm it.
Do not reject a claim for being "speculative" or "depends on runtime state" when that state is realistic: races, a failing command under `set -e`,
null on an error path, a cold cache, a boundary the code does not exclude, a partial failure are PLAUSIBLE.
REJECTED only when you can show it: the code contradicts the claim (quote the line), the case is provably impossible (type, constant, invariant),
the diff already guards it (cite the guard), or it is pure style with no observable effect. Say where.
UNCLEAR means you could not decide with the files available. Nothing else."""


def one_reviewer(context: str, security: bool) -> tuple[str, str]:
    """All the angles as one checklist for ONE reviewer. Claude Code's own /code-review gets nine findings out of a 22-file PR from a
    single agent in under a minute; what matters is the method and the budget, not how many agents there are."""
    parts = [LENSES[0], LENSES[1], LENSES[2], LENSES[3] if "PROJECT RULES" in context else LENSES[4], *([SECURITY_LENS] if security else [])]
    return ("review", "Work through every angle below in turn, spending most of your effort where the diff is riskiest:\n\n" + "\n\n".join(t for _, t in parts))


TRIAGE_PROMPT = """You are planning a code review, not doing it. Read the change below and decide what is worth checking, so the reviewers
spend their effort there instead of re-reading everything.

{context}DIFF ({files}):
{diff}

List the specific things in THIS change that could break something: a changed condition, a deleted or moved guard, error path or exit whose job the new code may not do any more, a new awaited call or
write, a changed signature or shape someone else consumes, a command that now runs somewhere it did not, a rule from the project notes it may break.
You may grep or read a file to see who consumes something, but do not review. Skip what only moves text around (comments, copy, formatting).
Reply with at most 6 lines, EXACTLY:
CHECK | path:line | the question a reviewer must answer by reading code (name the consumer or caller to read when you know it)
If nothing in the change can plausibly break anything, reply: NONE"""
TRIAGE_CHUNK = 3  # questions per reviewer: a reviewer holding more than this drifts into skimming all of them


def parse_triage(reply: str) -> list[str]:
    return [f"{m.group(1).strip()}: {m.group(2).strip()}" for m in re.finditer(r"(?im)^\W*CHECK\s*\|\s*([^|\n]+?)\s*\|\s*(.+)$", reply)][:6]


def triage_lenses(checks: list[str]) -> list[tuple[str, str]]:
    """The questions the triage picked, dealt out TRIAGE_CHUNK at a time. Three questions are one reviewer; six are two."""
    out = []
    for i in range(0, len(checks), TRIAGE_CHUNK):
        rows = "\n".join(f"- {c}" for c in checks[i:i + TRIAGE_CHUNK])
        out.append(("review" if i == 0 else "focus", "WHAT TO CHECK (a triage read this diff and picked these; answer each by reading the code, "
                    "then report anything else this diff breaks that they missed):\n" + rows))
    return out


async def triage(config, role: str, diff: str, files: list[str], context: str) -> list[str]:
    reply = await ask(config, TRIAGE_PROMPT.format(context=context, files=", ".join(files[:25]), diff=diff), role, max_turns=4,
                      max_tokens=min(config.review_pipeline_tokens, 25_000), disabled_skills=["code-review"])
    return parse_triage(reply)


def is_light(diff: str, files: list[str], secure: bool) -> bool:
    """A change this small needs no plan: up to two files, under SMALL_DIFF_LINES of diff, nothing touching auth, queries or money.
    Example: a 16-line edit to one workflow file gets one reviewer, not a triage call plus several."""
    return len(files) <= 2 and diff.count("\n") < SMALL_DIFF_LINES and not secure


def pick_lenses(config, diff: str, files: list[str], context: str) -> list[tuple[str, str]]:
    """Auto (review_lenses = 0) is ONE reviewer carrying every angle plus one that only follows producers to consumers. review_lenses = N >= 2 fans out to N separate angle reviewers
    (security is appended when auth, queries or money are touched); 1 pins the behaviour angle alone."""
    secure = bool(SECURITY_HINT.search(" ".join(files) + "\n" + diff))
    if config.review_lenses == 0:
        if len(files) == 1 and diff.count("\n") < SMALL_DIFF_LINES:
            return [one_reviewer(context, secure)]
        return [one_reviewer(context, secure), CONTRACT_LENS]
    picked = LENSES[: config.review_lenses]
    if config.review_lenses != 1 and secure:
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
    repro: str = ""  # what running the code showed: "ran X: <output line>", or that it could not be made to happen
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


def frame(answer: str, summary: str, reviewed: list[str], moves: list[tuple[str, str]], deleted: list[str], checks: list | None = None, strengths: list[str] | None = None, aside: list[str] | None = None, scope: str = "") -> str:
    """What a human reviewer writes around the findings: what the change does, what was and was not looked at.
    Without it "No issues found." says nothing about how much was read."""
    lines = [f"**What the change does:** {tidy(summary, 320)}."] if summary else []
    good = [tidy(x, 200) for x in (strengths or []) if tidy(x, 200)]
    if good:
        lines.append(f"**What is good:** {good[0]}.")
    lines.append(answer)
    seen = [f"{len(reviewed)} file(s)"]
    if moves:
        seen.append(f"{len(moves)} moved unchanged (" + ", ".join(f"{a} -> {b}" for a, b in moves[:3]) + (", ..." if len(moves) > 3 else "") + ")")
    if deleted:
        seen.append(f"{len(deleted)} deleted")
    bad = [c.command for c in checks or [] if not c.ok]
    ran = (f"Checks that FAILED: {', '.join(bad)}." if bad else f"{len(checks)} project check(s) passed (typecheck, lint, tests).") if checks else "I did not run typecheck, lint or tests."
    lines.append("_" + (f"Scope: {scope} " if scope else "") + "I read " + ", ".join(seen) + ". " + ran + (" Stage the move with git add -A on both paths so git keeps the rename." if moves else "") + "_")
    return "\n\n".join(lines)


def split_clipped(files: list[str]) -> list[tuple[str, list[str]]]:
    """A diff too big for one prompt is cut by area; a change that lives in one area is cut into thirds, so no reviewer is told to go and
    read what the prompt had to drop."""
    groups = group_files(files)
    if len(groups) == 1 and len(files) > 1:
        return [(f"part {i + 1}", files[i::MAX_GROUPS]) for i in range(min(MAX_GROUPS, len(files)))]
    return groups


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
    for m in re.finditer(r"^\W*(\d+)\s*\|\s*(CONFIRMED|PLAUSIBLE|REJECTED|UNCLEAR)\s*\|(.*)$", text or "", re.I | re.M):
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
    live = sorted((f for f in findings if f.verdict in ("CONFIRMED", "PLAUSIBLE", "UNCLEAR", "UNCHECKED")), key=lambda f: SEVERITY_ORDER[shown_severity(f)])
    rejected = [f for f in findings if f.verdict == "REJECTED"]
    lines = [f"{reviewers} reviewer(s) read the change in parallel, each through its own angle; each finding was then checked by a separate agent that tried to refute it."]
    if not live and not rejected:
        lines.append("No reviewer found a defect.")
    for f in live:
        tag = "confirmed" if f.verdict == "CONFIRMED" else "unverified"
        lines.append(f"- [{tag}] {shown_severity(f)} {f.where}  {f.what} -> {f.fix}" + (f"  (evidence: {f.evidence})" if f.evidence else "") + (f"  (affects: {f.reach})" if f.reach and f.reach != "n/a" else "") + (f"  (repro: {f.repro})" if f.repro else ""))
    for f in rejected:
        lines.append(f"- [rejected] {f.where}  {f.what[:90]}  because: {f.evidence or 'the code contradicts it'}")
    return "\n".join(lines)


def drop_missing_files(findings: list[Finding], files: list[str]) -> list[Finding]:
    """A finding that cites a file that is not in the repo (and not one the change deleted) is a reviewer's slip, not a defect to hand the
    reader: `EventTabNav.tsx:30 (file not found)` sends them looking for something that is not there."""
    return [f for f in findings if "(file not found)" not in f.where or f.where.split(":")[0].strip().replace(" (file not found)", "") in files]


def apply_verdicts(batch: list[Finding], reply: str, min_confidence: int, position: dict[int, int]) -> None:
    """`position` maps a finding to its number in the whole list: SAME_AS in a reply counts inside the batch it saw."""
    for f, v in zip(batch, parse_verdicts(reply, len(batch)).values()):
        f.verdict, f.evidence, f.reach, f.temporary, f.at_head, f.same_as, f.confidence = v
        if f.same_as:
            f.same_as = position[id(batch[f.same_as - 1])] if f.same_as <= len(batch) else 0
        if f.verdict != "REJECTED" and f.at_head:  # the old code failed the same way: not this change's defect
            f.verdict, f.evidence = "REJECTED", "already present before this change"
        if f.verdict not in ("REJECTED", "PLAUSIBLE") and f.confidence < min_confidence:  # a plausible claim is low confidence by definition
            f.verdict, f.evidence = "REJECTED", f"low confidence ({f.confidence}/100): {f.evidence}"


def claim_lines(batch: list[Finding]) -> str:
    return "\n".join(f"{n}. {f.severity} | {f.where} | {f.what} | fix: {f.fix or '(none)'}" for n, f in enumerate(batch, 1))


async def verify(config, role: str, findings: list[Finding]) -> None:
    """Claims go to verifiers six at a time (in parallel), so each claim gets turns of its own. A claim the verifier could not decide gets one
    more look from a fresh verifier with a bigger budget; if it is still undecided it stays as PLAUSIBLE (shown as unverified, at most MEDIUM),
    because only a refutation the verifier can show should remove a claim. Example: "the extra fetch is redundant" stays UNCLEAR until someone opens the
    detail query and finds what it returns that the list payload lacks."""
    position = {id(f): n for n, f in enumerate(findings, 1)}
    batches = [findings[i:i + VERIFY_BATCH] for i in range(0, len(findings), VERIFY_BATCH)]

    async def one(batch: list[Finding], turns: int, note: str = "") -> None:
        reply = await ask(config, note + VERIFIER_PROMPT.format(claims=claim_lines(batch)), role, max_turns=turns, max_tokens=config.review_pipeline_tokens, disabled_skills=["code-review"])
        apply_verdicts(batch, reply, config.review_min_confidence, position)

    await asyncio.gather(*(one(b, min(30, 10 + 3 * len(b))) for b in batches))
    unclear = [f for f in findings if f.verdict == "UNCLEAR"]
    if unclear:
        await one(unclear, 30, "A first verifier could not decide these. Settle each: open the files it would have needed, follow the call chain to the end, and answer CONFIRMED or REJECTED.\n\n")
        for f in unclear:
            if f.verdict == "UNCLEAR":
                f.verdict, f.evidence = "PLAUSIBLE", "could not be settled by reading the code"  # only a proven refutation drops a claim


async def reproduce(config, role: str, cwd: Path, findings: list[Finding], files: list[str], target) -> None:
    """Run the confirmed HIGH findings in a throwaway copy of the change. A defect that shows up in output is the strongest evidence there is;
    one the real code path refuses to show is capped at MEDIUM, with what was tried written next to it."""
    from agent.review_repro import MAX_REPROS, REPRO_PROMPT, REPRO_TOOLS, drop_scratch, link_dependencies, make_scratch, parse_repro
    from config.config import Config

    picks = [f for f in findings if f.verdict == "CONFIRMED" and shown_severity(f) in ("CRITICAL", "HIGH")][:MAX_REPROS]
    if not picks:
        return
    own = not (target and target.cwd)  # a PR worktree is already a throwaway
    if own:
        committed = bool(target) and not target.includes_worktree
        scratch = await make_scratch(cwd, files, target.rev[-1] if committed else "HEAD", from_worktree=not committed)
    else:
        scratch = target.cwd
        link_dependencies(target.repo or cwd, scratch)
    if not scratch:
        return
    try:
        scfg = Config(**{**config.to_dict(), "cwd": scratch})

        async def one(f: Finding) -> None:
            reply = await ask(scfg, REPRO_PROMPT.format(claim=f"{f.where} | {f.what}", evidence=f.evidence or "(none)"), role, 20, config.review_pipeline_tokens, tools=REPRO_TOOLS, approval="auto")
            word, rest = parse_repro(reply)
            if word == "REPRODUCED":
                f.repro = f"reproduced: {rest}"
            elif word == "NOT_REPRODUCED":
                f.repro, f.severity = f"tried to reproduce and could not: {rest}", "MEDIUM" if f.severity in ("CRITICAL", "HIGH") else f.severity

        await asyncio.gather(*(one(f) for f in picks))
    finally:
        if own:
            await drop_scratch(cwd, scratch)


def tidy(text: str, limit: int = 260) -> str:
    """One readable sentence or two: model debris after a stray tag is cut, long text is clipped at a word."""
    text = re.split(r"<tool_call|\]<|\[<", text or "")[0].strip().rstrip(".;, ")
    if len(text) <= limit:
        return text
    return text[:limit].rsplit(" ", 1)[0] + "..."


def headline(what: str) -> tuple[str, str]:
    """(title, rest): the first sentence is the heading and the remainder the explanation. With no sentence break, the first clause
    ("..., so ...", "...; ...", "... - ...") is, and a short finding is all heading."""
    text = tidy(what, 700)
    m = re.match(r"(.{20,170}?[.!?])\s+(.*)", text, re.S)
    if m:
        return m.group(1).rstrip("."), m.group(2).strip()
    if len(text) <= 190:
        return text, ""
    m = re.match(r"(.{40,170}?)(?:\s[-\u2014]+\s|;\s|:\s|,\s)(.*)", text, re.S)
    if m:
        return m.group(1), m.group(2).strip()
    cut = text[:170].rsplit(" ", 1)[0]
    return cut, text[len(cut):].strip()


def display_order(findings: list[Finding]) -> list[Finding]:
    """The findings in the order the answer numbers them (proven blockers, proven fixes, minor, then guesses). "Fix 2" means item 2 of that list."""
    live = sorted((f for f in findings if f.verdict != "REJECTED"), key=lambda f: (f.verdict != "CONFIRMED", SEVERITY_ORDER[shown_severity(f)], f.where))
    proven = [f for f in live if f.verdict == "CONFIRMED"]
    blocking = [f for f in proven if shown_severity(f) in ("CRITICAL", "HIGH")]
    rest = [f for f in proven if shown_severity(f) == "MEDIUM"]
    minor = [f for f in proven if f not in blocking and f not in rest]
    return [*blocking, *rest, *minor, *(f for f in live if f.verdict != "CONFIRMED")]


def format_answer(findings: list[Finding]) -> str:
    """The review a user reads, built from the verified findings with no model call. Every finding that survived is shown,
    nothing is dropped for length; they are laid out so they are quick to read (markdown, which the terminal renders):

        ## Code review: 2 issues

        ### Likely bugs
        **1. `a.ts:4` The insert can throw and 500 the whole list**
        - **Why:** ...
        - **Affects:** every visitor
        - **Fix:** catch it

    Heading = first sentence of the finding, bullets = the rest, the fix and who is hit.
    """
    live = sorted((f for f in findings if f.verdict != "REJECTED"), key=lambda f: (f.verdict != "CONFIRMED", SEVERITY_ORDER[shown_severity(f)], f.where))
    ruled = sum(f.verdict == "REJECTED" for f in findings)
    if not live:
        return "## Code review\n\nNo issues found." + (f" I checked {ruled} suspected problem(s) and the code ruled them out." if ruled else "")
    proven = [f for f in live if f.verdict == "CONFIRMED"]
    blocking = [f for f in proven if shown_severity(f) in ("CRITICAL", "HIGH")]
    rest = [f for f in proven if shown_severity(f) == "MEDIUM"]
    minor = [f for f in proven if f not in blocking and f not in rest]
    guess = [f for f in live if f.verdict != "CONFIRMED"]
    out = [f"## Code review: {len(live)} issue{'s' if len(live) != 1 else ''}"]
    n = 0
    for title, group in (("Likely bugs", blocking), ("Worth fixing", rest), ("Minor", minor), ("Not verified, worth a look", guess)):
        if not group:
            continue
        lines = [f"### {title}"]
        for f in group:
            n += 1
            head, more = headline(f.what)
            lines.append(f"**{n}. `{f.where}`  {head}**")
            if more:
                lines.append(f"- **Why:** {more.rstrip('.')}.")
            if f.reach and f.reach != "n/a":
                lines.append(f"- **Affects:** {tidy(f.reach, 160)}")
            if f.temporary:
                lines.append(f"- **Temporary:** goes away by itself ({tidy(f.temporary, 80)})")
            if f.fix:
                lines.append(f"- **Fix:** {tidy(f.fix, 400).rstrip('.')}")
            if f.repro:
                lines.append(f"- **Repro:** {tidy(f.repro, 300)}")
            lines.append("")
        out.append("\n".join(lines).rstrip())
    if blocking:
        out.append(f"**Before merging:** fix {'item' if len(blocking) == 1 else 'items'} {', '.join(str(i) for i in range(1, len(blocking) + 1))}. Want me to fix {'it' if len(blocking) == 1 else 'them'}?")
    else:
        out.append("**None of these block the merge.** Want me to fix any of them?")
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


async def ask(config, prompt: str, role: str, max_turns: int, max_tokens: int, disabled_skills: list[str] | None = None,
              tools: list[str] | None = None, approval: str = "never") -> str:
    """One read-only sub-agent with its own context; returns its last text. A crash or a stop is an empty answer, never an error."""
    from agent.agent import Agent
    from agent.events import AgentEventType
    from agent.review_claude import ask_claude, available
    from config.config import Config

    if config.review_backend == "claude-code" and available() and not tools:  # a repro needs a shell, which the read-only Claude sessions do not get
        answer, tokens = await ask_claude(config, prompt)
        if answer.strip():
            _spent_log.append(tokens)
            return answer
        # an empty answer (not logged in, timeout) falls through to a melon reviewer instead of losing the review

    cfg = Config(**{
        **config.to_dict(),
        "model_role": role,
        "allowed_tools": tools or ["read_file", "grep", "glob", "list_dir", "graph", "search", "outline", "git", "use_skill"],
        "auto_review": False, "auto_verify": False, "verify_commands": [], "verify_rules": [], "subagents": [],
        "enable_parallel_tasks": False, "mcp_servers": {}, "review_pipeline": False, "auto_challenge": False, "auto_scout": False, "sub_agent": True, "auto_route": False,  # the prompt is a review brief, not a user request: its words ("authorization", "security") must not send it to the strong tier
         # a challenger per sub-agent cost 80k tokens and 4 minutes
        "max_turns": max_turns, "max_total_tokens": max_tokens, "max_seconds": 300, "approval": approval,
        "disabled_skills": [*config.disabled_skills, *(disabled_skills or [])],
    })
    texts: list[str] = []

    async def run_it() -> None:
        async with Agent(cfg) as sub:
            async for ev in sub.run(prompt):
                if ev.type == AgentEventType.TEXT_COMPLETE and ev.data.get("content"):
                    texts.append(ev.data["content"])
            _spent_log.append(sub._spent())

    try:
        # the agent's own 300s budget only covers model waits; a stall anywhere else (startup, a backup, closing a stream) must not hang the review
        await asyncio.wait_for(run_it(), timeout=cfg.max_seconds + 60)
    except Exception:  # a broken or stalled reviewer must not stop the review (a timeout is an empty answer too)
        return texts[-1] if texts else ""
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
    """Resolve what to review, run it, and remove the throwaway worktree a PR from another branch was checked out in."""
    from config.config import Config

    target = await review_target.resolve(Path(config.cwd), request, SKIP_SUFFIXES)
    if target is None and NAMED_SCOPE.search(request):
        return  # it names a commit/PR/branch we cannot read from here: the normal review answers
    try:
        if target and target.cwd:
            config = Config(**{**config.to_dict(), "cwd": target.cwd})  # reviewers, checks' rules and `git show` all read the PR's tree
        async for item in _run_review(config, request, target):
            yield item
    finally:
        if target:
            await target.cleanup()


async def _run_review(config, request: str, target) -> AsyncGenerator[tuple[str, str], None]:
    """Yields ("notice", text) as it goes and finally ("result", block), or nothing when there is nothing to review.
    Reviewers each take one angle on the change (more of them for a bigger or riskier diff), the project's checks run beside
    them, a last reviewer hunts for what they all missed, then ONE verifier tries to refute every claim and scores it."""
    cwd = Path(config.cwd)
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
    # a diff past the 40k prompt cap is still one reviewer's job (it reads the rest with `git diff`); only a really wide change
    # (30+ files) is cut into areas, and then each area gets one all-angle reviewer, not four
    groups = split_clipped(files) if "[diff clipped]" in whole and len(files) > SPLIT_FILES else [("the whole change", files)]
    if target:
        for note in target.notes:
            yield "notice", f"WARNING scope: {note}"
        if target.commits:
            yield "notice", "Commits covered: " + "; ".join(target.commits[:6]) + (f"; ... ({len(target.commits)} in all)" if len(target.commits) > 6 else "")
    yield "notice", f"Reviewing {len(files)} changed file(s)" + (f" in {label}" if target else "") + (f" and {len(moves)} moved unchanged" if moves else "")
    role = config.review_pipeline_role
    stayed = await left_behind(cwd, moves)
    _spent_log.clear()

    check_task = asyncio.create_task(run_checks(config, files)) if config.review_run_checks and (target is None or (target.includes_worktree and not target.cwd)) else None  # a fresh PR worktree has no installed dependencies
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
    lenses = pick_lenses(config, whole, files, context) if len(groups) == 1 else [one_reviewer(context, bool(SECURITY_HINT.search(" ".join(files) + "\n" + whole)))]
    light = False
    if config.review_lenses == 0 and len(groups) == 1 and is_light(whole, files, bool(SECURITY_HINT.search(" ".join(files) + "\n" + whole))):
        light = True
        lenses = [(n, t + LIGHT_NOTE) for n, t in [one_reviewer(context, False)]]
        yield "notice", "Small change: one quick reviewer, no triage and no verifier"
    elif config.review_lenses == 0 and len(groups) == 1:
        checks = await triage(config, role, whole, files, context)
        if checks:
            lenses = triage_lenses(checks)
            yield "notice", f"Triage: {len(checks)} thing(s) worth checking, {len(lenses)} reviewer(s) assigned"
        else:
            yield "notice", "Triage found nothing specific; one reviewer reads the whole change"
            lenses = [one_reviewer(context, bool(SECURITY_HINT.search(" ".join(files) + "\n" + whole)))]
    clipped = "[diff clipped]" in whole  # a split diff already multiplies the reviewers
    if len(groups) * len(lenses) > 1:
        angles = ", ".join("every angle" if n == "review" else n for n, _ in lenses)
        yield "notice", f"{len(groups) * len(lenses)} reviewers in parallel" + (f", one per part ({len(groups)} parts)" if len(groups) > 1 else f": {angles}") + (" (rules and playbooks loaded)" if "PROJECT RULES" in context or "PLAYBOOKS" in context else "")
    backend = " through Claude Code" if config.review_backend == "claude-code" else ""
    if backend:
        yield "notice", "Reviewers run as Claude Code sessions (read-only)"

    async def review(name: str, fs: list[str], lens: str, turns: int = 12) -> list[Finding] | None:
        diff = whole if len(groups) == 1 else await build_diff(cwd, fs, rev)
        others = ", ".join(f for f in files if f not in fs)[:600] or "(none)"
        scope = label if len(groups) == 1 else f"one part ({name}) of {label} that {len(groups)} reviewers split"
        if clipped and len(groups) == 1:
            scope += f" (the diff below is cut short: for the files it does not reach, run `git diff {rev[0]} -- <file>` and read each one before you finish)"
        if target and not target.includes_worktree:
            scope += f" (a committed range: read old code with `git show {rev[0]}:<file>` and new code with `git show {rev[1]}:<file>`)"
        moved = ("MOVED WITHOUT ANY CHANGE (identical bytes, so there is nothing in them to review; check that imports and paths still resolve): "
                 + "; ".join(f"{a} -> {b}" for a, b in moves)
                 + "\nSTILL IN THE OLD FOLDER (did any of these rely on what moved, e.g. inherit its loading.tsx, layout or index export? say what they lose, or that they are fine): "
                 + (", ".join(stayed) or "(nothing)") + "\n") if moves else ""
        prompt = REVIEWER_PROMPT.format(scope=scope, request=request[:1500], part=", ".join(fs[:25]), moved=moved, others=others, context=context, diff=diff, lens=lens)
        reply = await ask(config, prompt, role, max_turns=turns, max_tokens=config.review_pipeline_tokens)
        summaries.extend(re.findall(r"(?im)^\W*SUMMARY:\s*(.+)$", reply))
        strengths.extend(re.findall(r"(?im)^\W*STRENGTH:\s*(.+)$", reply))
        aside.extend(re.findall(r"(?im)^\W*ASIDE:\s*(.+)$", reply))
        return parse_findings(reply) if reply.strip() else None

    summaries: list[str] = []
    strengths: list[str] = []
    aside: list[str] = []
    replies = await asyncio.gather(*(review(n, fs, lens, LIGHT_TURNS if light else 30 if lname in ("review", "contracts") else 20) for n, fs in groups for lname, lens in lenses))
    review_tokens = sum(_spent_log)
    if all(r is None for r in replies):
        yield "notice", "The reviewer gave no answer; reviewing without the pipeline"
        return  # an empty answer is a failure, not "no defects": the normal review loop takes over
    found = [f for part in replies if part for f in part]
    found = merge_duplicates(sorted(found, key=lambda f: SEVERITY_ORDER[f.severity]))

    if config.review_gap_pass and len(groups) == 1 and len(lenses) >= 2 and whole.count("\n") >= GAP_MIN_LINES:
        yield "notice", "A last reviewer hunts for what the others missed"
        reported = "\n".join(f"- {f.severity} {f.where} {f.what}" for f in found) or "(nothing)"
        extra_found = await review("the whole change", files, GAP_LENS.format(found=reported), 20) or []
        found = merge_duplicates(sorted([*found, *extra_found], key=lambda f: SEVERITY_ORDER[f.severity]))
        review_tokens = sum(_spent_log)

    await fix_paths(found, cwd)
    found = drop_missing_files(found, files)
    to_check, extra = found[:MAX_VERIFIED], found[MAX_VERIFIED:]
    if to_check and not light:  # a quick pass on a small change is shown as the reviewer's claims, marked unverified, like Claude Code's low effort
        yield "notice", f"Verifying {len(to_check)} finding(s); the verifier tries to refute and score each"
        await verify(config, role, to_check)
    found = merge_same_cause(to_check) + extra
    if config.review_repro and any(f.verdict == "CONFIRMED" for f in found):
        yield "notice", "Running the confirmed defects in a throwaway copy to see them happen"
        try:
            await reproduce(config, role, cwd, found, files, target)
        except Exception:  # a repro that cannot start leaves the verified finding as it was
            pass
    late = check_task.result() if check_task and check_task.done() and not checks and not check_task.cancelled() else checks
    if check_task and not check_task.done():
        check_task.cancel()
    found = check_findings(late, files) + found  # a failing typecheck on a changed file needs no model to confirm
    review_memory.remember(cwd, [(f.where, f.what, f.evidence) for f in found if f.verdict == "REJECTED" and not f.evidence.startswith(("low confidence", "already present", "could not be settled"))])
    confirmed = sum(f.verdict == "CONFIRMED" for f in found)
    rejected = sum(f.verdict == "REJECTED" for f in found)
    verify_tokens = sum(_spent_log) - review_tokens
    yield "notice", f"Verified: {confirmed} confirmed, {rejected} rejected, {len(found) - confirmed - rejected} unverified"
    yield "notice", f"Tokens: reviewers {review_tokens:,} + verifier {verify_tokens:,} = {review_tokens + verify_tokens:,}"
    scope = f"{label}" + (" (" + "; ".join(target.notes) + ")" if target and target.notes else "") + "." if target else ""
    yield "findings", display_order(found)
    yield "answer", frame(format_answer(found), summaries[0].strip() if summaries else "", files, moves, deleted, late, strengths[:2], aside[:3], scope)
    yield "result", compose(found, len(groups) * len(lenses))
