"""Requests that cover many places ("fix all such issues in the whole app") need a sweep, not a few lucky reads.

A saved session shows what happens without one: 7 files looked at, 4 fixed, answer written, while the same button
pattern sat in dozens of other components. The sweep makes mew list every place first, then close the list."""
from __future__ import annotations

import re

BROAD = re.compile(
    r"\b(whole|entire|full)\s+(app|application|codebase|code base|repo|repository|project|site|website|web)\b"
    r"|\b(all|every|each)\s+(such|the|these|those|other|similar|of the|of these)?\s*"
    r"(issues?|bugs?|places?|pages?|components?|routes?|files?|buttons?|forms?|screens?|endpoints?|handlers?|usages?|instances?|cases?|occurrences?)\b"
    r"|\beverywhere\b|\bacross the (app|codebase|repo|project)\b|\bany(?:where)? (similar|such|other)\b",
    re.I,
)


def is_broad(request: str) -> bool:
    return bool(BROAD.search(request))


SWEEP = (
    "This request covers many places, so sweep before you fix. (1) Search the area the user named, repo-wide and with several "
    "patterns, and list EVERY place that could have the problem (grep for the pattern, `graph importers` for who uses a shared "
    "component). (2) Write that list with the todos tool, one step per place or group of places. (3) Work through it: for each "
    "step either fix it or note in one line why it is fine. (4) Before you answer, search again and confirm nothing on the list "
    "is left. Answer in a few lines: what you changed, and what you ruled out as one short group. Do not stop after the first few files."
)

SWEEP_CHECKPOINT = (
    "Checkpoint: update the todos with what you have covered so far (mark done what you fixed or ruled out) and keep going down "
    "the list. Do not stop while steps are open."
)


def unfinished(todos_tool) -> list[str]:
    """Open steps of the todos list, empty when there is none or everything is done."""
    items = getattr(todos_tool, "_todos", None) or {}
    return [t["content"] for t in items.values() if not t["done"]]
