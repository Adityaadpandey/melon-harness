"""Fixing what a review found, the way a senior engineer does it instead of the way a quick patch does.

After a review the user says "fix them", "fix 1 and 3" or "review and fix". The findings of that review are still in memory, so the
fix run starts from the facts the reviewers and the verifier already established, not from a vague "fix the issues":

    1. a brief per finding: where, why it breaks, what the verifier read, the fix that was suggested, the repro command if there is one
    2. the working rules Claude Code gives its implementation workers: root cause not symptom, check every caller and the mirrored half,
       smallest diff, a test that fails before and passes after, run the project's checks and read the failures, say what "done" means
    3. the advisor is consulted before the first edit and again before finishing (the registry refuses edits until it has been)
    4. a fresh verifier and a reviewer judge the result (agent/review_fix_verify.py), and what they find goes back to the author"""
from __future__ import annotations

import re

NEGATED = re.compile(r"\b(don'?t|do not|no need to|without|never)\s+(\w+\s+)?fix", re.I)
FIX_WORD = re.compile(r"\bfix(ing|es|ed)?\b", re.I)
# "fix them", "yes fix it", "go ahead and fix these", "fix all", "fix the issues/findings/bugs", "fix 1 and 3", "fix #2"
FIX_REPLY = re.compile(
    r"^\W*(?:(?:yes|yep|yeah|ok(?:ay)?|sure|please|go ahead(?: and)?|do it|and)\b[\s,.!]*)*fix\b[\s\w#,&-]*?"
    r"(?:\b(?:them|it|these|those|all|everything|the (?:issues?|findings?|bugs?|problems?|items?))\b|#?\d)", re.I)
NUMBERS = re.compile(r"#?\b(\d{1,2})\b")


def asks_for_fix(message: str) -> bool:
    """A reply that is mostly "fix <the findings>": short, starts with fix (or yes/ok/go ahead and fix)."""
    return len(message) < 200 and not NEGATED.search(message) and bool(FIX_REPLY.search(message))


def review_and_fix(message: str) -> bool:
    """One message that asks for both ("review this PR and fix what you find")."""
    return bool(FIX_WORD.search(message)) and not NEGATED.search(message)


def chosen(message: str, shown: list) -> list:
    """The findings the user pointed at: "fix 1 and 3" is items 1 and 3 of the list the answer showed; anything else is every proven one.
    Guesses (unverified, unclear) are only fixed when named by number."""
    numbers = [int(n) for n in NUMBERS.findall(message) if 0 < int(n) <= len(shown)]
    if numbers:
        return [shown[n - 1] for n in dict.fromkeys(numbers)]
    return [f for f in shown if f.verdict == "CONFIRMED"]


RULES = """HOW TO FIX THESE (work like the senior engineer who owns this code):
1. Read before you change. For each finding open the cited lines, then the callers, the consumers and the other half of any pair (desktop/mobile,
   create/update, server/client, cached/uncached). Fix the ROOT CAUSE once, where every path goes through it, not a guard in the one caller the
   finding names. If the suggested fix treats a symptom, say so and fix the cause.
2. Call the `advisor` tool BEFORE your first edit (after you have read the code): it sees this whole conversation and checks your diagnosis
   and plan. Weigh its advice seriously; if the code you read contradicts it, call advisor again with "I found X, you said Y, which wins?".
   The edit tools refuse to run until you have consulted it.
3. Smallest diff that is fully correct: match how the neighbouring code does the same thing (naming, comments, error handling), no drive-by
   cleanup, no new dependency, no reformatting of lines you do not need to touch. Deleting code beats adding a fallback.
4. Prove each fix. Where a repro is given, run it before and after. Otherwise add a regression test in the repo's own style that fails without
   your change and passes with it (look at the neighbouring tests for the runner and helpers). Then run the typecheck, lint and the tests that
   cover the files you touched. A failure is never "unrelated" until you have read it and shown why.
5. Before you say you are done: re-read your own diff (`git diff`) as a reviewer would, check every other place the same cause appears, then call
   `advisor` once more with the finished work. Make the work durable first (files saved).
6. Do not commit, push or touch files unrelated to the findings.

DONE MEANS: for every finding, the trigger in its description no longer produces the bad result, a test or repro shows it, the checks pass, and
nothing outside the finding changed behaviour.

FINAL ANSWER, one block per finding: `file:line` of the change, why this is the root cause (one sentence), how you verified it (the command and
what it printed). Findings you chose not to fix, or could not, get the reason. No other prose."""


def brief(findings: list) -> str:
    """The task a fix run is handed. Everything the review learned travels with it."""
    rows = []
    for n, f in enumerate(findings, 1):
        row = [f"{n}. [{f.verdict.lower()}] {f.severity} {f.where}", f"   problem: {f.what}"]
        if f.evidence:
            row.append(f"   what the verifier read: {f.evidence}")
        if f.reach and f.reach != "n/a":
            row.append(f"   affects: {f.reach}")
        if f.fix:
            row.append(f"   suggested fix: {f.fix}")
        if getattr(f, "repro", ""):
            row.append(f"   repro: {f.repro}")
        rows.append("\n".join(row))
    return "FINDINGS TO FIX (from the review you just gave; each was checked by a separate agent):\n\n" + "\n\n".join(rows) + "\n\n" + RULES


def short(findings: list) -> str:
    """The same findings as one line each, for the reviewer that later judges the fix."""
    return "\n".join(f"{n}. {f.severity} {f.where}: {f.what}" for n, f in enumerate(findings, 1))


ADVISOR_FIRST = "Consult the advisor before you edit anything: call the `advisor` tool now (no arguments). It reads this whole conversation and checks your diagnosis and plan."
ADVISOR_LAST = ("You changed code after your last advisor consultation (or never consulted it). Call the `advisor` tool now with the finished work, "
                "act on what it says (fix it, or say why the code you read contradicts it), then give the final answer.")


def edit_gate(registry, advisor):
    """The registry refuses edit/write_file while the advisor has not been consulted this request."""
    def gate(name: str) -> str | None:
        return ADVISOR_FIRST if name in ("edit", "write_file") and advisor.calls == 0 else None

    return gate


def needs_final_advice(registry, advisor, base: int) -> bool:
    """True when this request changed files (`base` is the registry's write count when it started) after the advisor was last consulted, or without ever consulting it."""
    return registry._writes > base and registry._writes > advisor.last_call_at
