"""Pure edit planning: given file text and requested replacements, produce the
new text or a precise error. Used for both the approval preview and the real
write, so what you approve is exactly what gets applied.

Matching is exact first. If that fails, a second pass ignores trailing
whitespace and typographic quotes/dashes (models often normalise those), and
applies only when it matches exactly one place. Line endings (CRLF) are kept."""
from __future__ import annotations

import difflib
import re
import unicodedata

_TRANS = str.maketrans({"‘": "'", "’": "'", "“": '"', "”": '"', "–": "-", "—": "-", " ": " "})


def _norm(line: str) -> str:
    return unicodedata.normalize("NFKC", line).translate(_TRANS).rstrip()


def _line_of(text: str, index: int) -> int:
    return text.count("\n", 0, index) + 1


def _find_all(text: str, needle: str) -> list[int]:
    out, i = [], text.find(needle)
    while i != -1:
        out.append(i)
        i = text.find(needle, i + len(needle))
    return out


def _fuzzy_span(text: str, old: str) -> list[tuple[int, int]]:
    """Spans of line windows equal to `old` after normalising each line."""
    lines = text.split("\n")
    want = [_norm(x) for x in old.strip("\n").split("\n")]
    if not any(want):
        return []
    starts = [0]
    for ln in lines[:-1]:
        starts.append(starts[-1] + len(ln) + 1)
    spans = []
    for i in range(len(lines) - len(want) + 1):
        if all(_norm(lines[i + k]) == want[k] for k in range(len(want))):
            end_line = i + len(want) - 1
            end = starts[end_line] + len(lines[end_line])
            if old.endswith("\n") and text[end:end + 1] == "\n":
                end += 1  # the caller's old text included the final newline, so consume it too
            spans.append((starts[i], end))
    return spans


def _hint(text: str, old: str) -> str:
    first = next((ln.strip() for ln in old.splitlines() if ln.strip()), "")
    lines = text.split("\n")
    near = [(i + 1, ln) for i, ln in enumerate(lines) if first and first in ln.strip()][:3]
    if not near:
        scored = sorted(
            ((difflib.SequenceMatcher(None, first, ln.strip()).ratio(), i + 1, ln) for i, ln in enumerate(lines) if ln.strip()),
            reverse=True,
        )[:2]
        near = [(i, ln) for r, i, ln in scored if r > 0.6]
    if near:
        return "\nClosest lines:\n" + "\n".join(f"  {i}: {ln.strip()[:100]}" for i, ln in near) + "\nRe-read the file and copy the text exactly."
    return "\nRe-read the file with read_file and copy the text exactly (whitespace and indentation matter)."


def plan_edits(content: str, edits: list[tuple[str, str]], replace_all: bool = False) -> tuple[str | None, str | None, int]:
    """Returns (new_content, error, replacements). Exactly one of the first two is None."""
    eol = "\r\n" if "\r\n" in content else "\n"
    text = content.replace("\r\n", "\n")
    spans: list[tuple[int, int, str]] = []

    for n, (old, new) in enumerate(edits, 1):
        label = f"edit {n}: " if len(edits) > 1 else ""
        old, new = old.replace("\r\n", "\n"), new.replace("\r\n", "\n")
        if not old:
            return None, f"{label}old_string is empty; use write_file to create or replace a file.", 0
        hits = [(i, i + len(old)) for i in _find_all(text, old)]
        if not hits:
            hits = _fuzzy_span(text, old)
            if len(hits) > 1:
                return None, f"{label}text matches {len(hits)} places ignoring whitespace/quote differences (lines {', '.join(str(_line_of(text, s)) for s, _ in hits[:5])}). Add surrounding lines to make it unique.", 0
        if not hits:
            return None, f"{label}old_string not found." + _hint(text, old), 0
        if len(hits) > 1 and not replace_all:
            where = ", ".join(str(_line_of(text, s)) for s, _ in hits[:6])
            return None, f"{label}old_string found {len(hits)} times (lines {where}). Add more context to make it unique, or set replace_all=true.", 0
        for s, e in (hits if replace_all else hits[:1]):
            spans.append((s, e, new))

    spans.sort()
    for (s1, e1, _), (s2, _e2, _) in zip(spans, spans[1:]):
        if s2 < e1:
            return None, "Edits overlap. Merge changes that touch the same or adjacent lines into one edit.", 0

    out = text
    for s, e, new in reversed(spans):
        out = out[:s] + new + out[e:]
    if out == text:
        return None, "No change: old_string equals new_string.", 0
    if eol == "\r\n":
        out = out.replace("\n", "\r\n")
    return out, None, len(spans)
