"""A fresh pair of eyes that tries to prove a fix works, the way Claude Code's verification workers do.

The author of a fix describes what it meant to do; the diff says what it did. The verifier never saw the author's reasoning, so it cannot
inherit its blind spots. It works in a throwaway copy of the change (shell on, OS sandbox on, node_modules linked) and answers:

    1 | FIXED | pnpm vitest a.test.ts: 4 passed; the same test fails on HEAD
    2 | NOT_FIXED | the retry path in b.ts:40 still throws on an empty list
    NEW | MEDIUM | c.ts:12 | the fix drops the empty-state branch the old code had
    VERDICT: FAIL

FAIL goes back to the author with exactly those lines."""
from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path

PROMPT = """You are verifying a fix you did not write. Your current directory is a throwaway copy of the changed tree (the fix is applied there as
uncommitted changes on top of HEAD; edit nothing in the real repo, you cannot reach it).

THE FINDINGS THE FIX WAS FOR:
{findings}

WHAT THE AUTHOR SAYS IT DID (a statement of intent, not evidence; the diff is the evidence):
{claimed}

AUTOMATED CHECKS THE HARNESS ALREADY RAN: {checks}

Prove the fix works. Confirming that it exists proves nothing, and a rubber stamp undermines everything.
1. Read the real diff first (`git diff HEAD`). Does it do what the author says? Does it fix the ROOT CAUSE, or only the place the finding named
   (grep every other caller, sibling and mirrored half for the same cause)? Is anything in the diff unrelated to the findings, a new dependency,
   or out of step with how the neighbouring code is written?
2. For each finding, make its trigger happen and show the result: run the repro if one is given, or the test the author added, or write the
   smallest script of your own (a file named .melon-verify.<ext>) that pushes the finding's input through the real code.
3. If the author added a test, show that it fails WITHOUT the fix: `git stash`, run it, `git stash pop` (stash only tracked changes; keep the test file).
4. Run the project's typecheck, lint and the tests that cover the changed files with the change in place. Investigate every error: nothing is
   "unrelated" until you have shown it fails the same way on HEAD.
5. Try what the author probably did not: empty and null inputs, the error path, the other caller, old data, the second call in a row.
No network, no installs, no database. A check you cannot run is said so, never assumed to pass.

Reply with exactly these lines and nothing else:
<finding number> | FIXED or NOT_FIXED or PARTIAL | the command you ran and the output line that decides it
NEW | HIGH or MEDIUM or LOW | path:line | a defect or regression the fix itself introduced, or a fix-quality problem (symptom patched, mirror missed, test missing) | the smallest correction
VERDICT: PASS or VERDICT: FAIL
PASS only when every finding is FIXED with evidence you produced and there is no NEW HIGH or MEDIUM line."""


@dataclass
class FixVerdict:
    passed: bool
    findings: list[tuple[int, str, str]] = field(default_factory=list)  # (number, FIXED|NOT_FIXED|PARTIAL, evidence)
    new: list[str] = field(default_factory=list)  # "SEVERITY path:line problem -> fix"
    ran: bool = True  # False when no verifier could run (no scratch copy, no answer): the review alone decides

    def feedback(self) -> str:
        rows = [f"{n}. {state}: {why}" for n, state, why in self.findings if state != "FIXED"]
        return "\n".join([*rows, *(f"NEW {x}" for x in self.new)]) or "(no detail)"

    def summary(self) -> str:
        done = sum(state == "FIXED" for _, state, _ in self.findings)
        return f"{done}/{len(self.findings)} finding(s) shown fixed" + (f", {len(self.new)} new problem(s)" if self.new else "")


def parse(text: str) -> FixVerdict:
    findings: list[tuple[int, str, str]] = []
    new: list[str] = []
    for line in (text or "").splitlines():
        parts = [p.strip() for p in line.strip().lstrip("-*• ").split("|")]
        if len(parts) >= 3 and parts[0].isdigit() and parts[1].upper() in ("FIXED", "NOT_FIXED", "PARTIAL"):
            findings.append((int(parts[0]), parts[1].upper(), parts[2][:300]))
        elif len(parts) >= 4 and parts[0].upper() == "NEW" and parts[1].upper() in ("HIGH", "MEDIUM", "LOW"):
            new.append(f"{parts[1].upper()} {parts[2]} {parts[3][:240]}" + (f" -> {parts[4][:160]}" if len(parts) > 4 and parts[4] else ""))
    verdict = re.findall(r"(?im)^\W*VERDICT:\s*(PASS|FAIL)\b", text or "")
    if not verdict:
        return FixVerdict(False, findings, new, ran=False)  # no verdict line: the verifier did not finish, which proves nothing either way
    bad = any(s != "FIXED" for _, s, _ in findings) or any(x.startswith(("HIGH", "MEDIUM")) for x in new)
    return FixVerdict(verdict[-1].upper() == "PASS" and not bad, findings, new)


async def verify_fix(config, role: str, cwd: Path, findings: str, claimed: str, files: list[str], checks: str) -> FixVerdict:
    """Run the verifier in a scratch copy. Never raises; a verifier that could not run returns ran=False and the review decides alone."""
    from agent.review_pipeline import ask
    from agent.review_repro import REPRO_TOOLS, drop_scratch, make_scratch
    from config.config import Config

    try:
        scratch = await make_scratch(cwd, files, "HEAD", from_worktree=True)
        if not scratch:
            return FixVerdict(False, ran=False)
        try:
            scfg = Config(**{**config.to_dict(), "cwd": scratch})
            reply = await ask(scfg, PROMPT.format(findings=findings, claimed=(claimed or "(nothing)")[:2500], checks=checks or "none ran"), role, 30,
                              config.review_pipeline_tokens, tools=REPRO_TOOLS, approval="auto")
        finally:
            await drop_scratch(cwd, scratch)
    except Exception:
        return FixVerdict(False, ran=False)
    return parse(reply)
