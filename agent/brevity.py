"""Keep the screen to what the user needs. Models narrate between tool calls ("Working notes before editing: ..."
and six bullets) even when told to write one short line. The full text stays in history and traces, so the model
keeps its notes; the user sees a single line."""
from __future__ import annotations

import re

HOLD_CHARS = 1500  # text this long with no tool call yet is a real answer: stop holding it back and stream it
LINE_CHARS = 140


def interim_line(text: str) -> str:
    """First meaningful line of text that came before a tool call, trimmed to one short line.
    Example: '- Working notes:\n- a.ts:3 ...' -> 'Working notes:'"""
    first = next((re.sub(r"\s+", " ", ln.strip(" \t-*•#>")) for ln in text.splitlines() if ln.strip(" \t-*•#>")), "")
    if len(first) <= LINE_CHARS:
        return first
    return first[:LINE_CHARS].rsplit(" ", 1)[0].rstrip(",;:") + "…"
