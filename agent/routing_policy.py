"""Pick a model tier for a request without spending a model call.

fast    - small, well-defined work (rename, typo, where is X, explain this)
default - the configured main model
strong  - debugging, concurrency, security, architecture, multi-file work

The classifier is deliberately simple and explainable (it returns its reason),
and the agent escalates to `strong` on evidence of trouble (a failed repair,
a detected loop) rather than guessing up front."""
from __future__ import annotations

import re

STRONG = re.compile(
    r"\b(debug\w*|root cause|race condition|concurren\w*|deadlock|refactor\w*|architect\w*|migrat\w*|"
    r"security|vulnerab\w*|auth\w*|payment\w*|webhook\w*|performance|optimi[sz]\w*|intermittent|flaky|"
    r"regression|why (does|is|are|do)|memory leak|transaction\w*|idempoten\w*|design|investigate)\b",
    re.I,
)
FAST = re.compile(
    r"\b(rename|typo|comment|format\w*|lint|where (is|are)|find|list|show|what (does|is)|explain|"
    r"read|open|print|bump|add (a )?(log|import)|remove (the )?(unused|log))\b",
    re.I,
)
PATHY = re.compile(r"[\w.\-]+/[\w./\-]+|\b[\w\-]+\.(?:ts|tsx|js|jsx|py|go|rs|java|sql|prisma)\b")


def classify(request: str) -> tuple[str, str]:
    """(tier, reason)"""
    text = request.strip()
    m = STRONG.search(text)
    if m:
        return "strong", f"'{m.group(0).lower()}' suggests a hard task"
    if len(text) > 700:
        return "strong", "long, detailed request"
    if len(PATHY.findall(text)) >= 4:
        return "strong", "touches several files"
    if len(text) < 200 and FAST.search(text) and not re.search(r"\band\b.*\band\b", text, re.I):
        return "fast", "small, well-defined request"
    return "default", "ordinary task"


# fast work needs little thinking, hard work gets the most; "default" leaves the model's own setting alone
EFFORT = {"fast": "low", "default": None, "strong": "high"}

REVIEWY = re.compile(r"\b(review|audit|critique)\b", re.I)
EXPLAINY = re.compile(r"\b(explain|walk me through|how does|what does|summari[sz]e)\b", re.I)


def length_hint(request: str, tier: str, will_change: bool) -> str:
    """One line appended to the user's message telling the model how long the answer should be.
    Debugging and other hard work gets no cap: cutting its reasoning would cost more than it saves."""
    if REVIEWY.search(request):
        return "Answer with at most 6 findings, each `severity path:line  what is wrong -> fix`; if there is no defect, only: No issues found."
    if EXPLAINY.search(request) and not will_change:
        return "Answer in under 300 words, with file:line references."
    if tier == "fast" and will_change:
        return "When done, reply in 1-2 sentences: what changed."
    return ""
