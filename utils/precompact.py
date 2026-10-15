"""Free cleanup of tool output before it goes to the summarizer, so no model tokens are spent on terminal noise.

Example: a 40-line pip progress bar, ANSI colours and the same lint error repeated 30 times
shrink to a few lines; the summary is no worse and the summarizer call is cheaper."""
import re

ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07")
PROGRESS = re.compile(r"(\d{1,3}%\|?[#=█▏▎▍▌▋▊▉ -]*|[#=█]{8,}|\.{10,})")
TRACE_START = re.compile(r"^Traceback \(most recent call last\):|^\s+at .+\(.+:\d+:\d+\)$")


def _squash_progress(lines: list[str]) -> list[str]:
    """A run of progress-bar lines (or one line redrawn with \\r) keeps only its last state."""
    out: list[str] = []
    for line in lines:
        line = line.rsplit("\r", 1)[-1]
        if PROGRESS.search(line) and out and PROGRESS.search(out[-1]):
            out[-1] = line
        else:
            out.append(line)
    return out


def _dedup(lines: list[str], keep: int = 3) -> list[str]:
    """The same line more than `keep` times (a lint rule firing everywhere) keeps the first few, then a count."""
    seen: dict[str, int] = {}
    out: list[str] = []
    dropped: dict[str, int] = {}
    for line in lines:
        key = re.sub(r"\d+", "N", line.strip())
        if len(key) < 12 or line.startswith((" ", "\t")):  # indented lines are stack frames, handled separately
            out.append(line)
            continue
        seen[key] = seen.get(key, 0) + 1
        if seen[key] <= keep:
            out.append(line)
        else:
            dropped[key] = dropped.get(key, 0) + 1
    if not dropped:
        return out
    return out + [f"[{sum(dropped.values())} more near-identical lines dropped]"]


def _trim_traces(lines: list[str], top: int = 3, bottom: int = 3) -> list[str]:
    """A stack of frames longer than top+bottom keeps its first and last frames."""
    out: list[str] = []
    i = 0
    while i < len(lines):
        j = i
        while j < len(lines) and (lines[j].startswith(("  File ", "    ", "\tat ")) or re.match(r"^\s+at ", lines[j])):
            j += 1
        if j - i > (top + bottom) * 2:  # python frames are two lines each
            n = (top + bottom) * 2
            out += lines[i:i + top * 2] + [f"  [{j - i - n} stack lines omitted]"] + lines[j - bottom * 2:j]
            i = j
        else:
            out.append(lines[i])
            i += 1
    return out


def pre_compact(text: str) -> str:
    if len(text) < 400:
        return text
    lines = ANSI.sub("", text).split("\n")
    return "\n".join(_dedup(_trim_traces(_squash_progress(lines))))
