"""Deterministic hallucination check for the final answer. Every file path (and
file:line) the model cites is checked against the real tree: a path that does
not exist, or a line past the end of the file, is a fabricated reference."""
from __future__ import annotations

import asyncio
import re
from dataclasses import dataclass
from pathlib import Path

EXTS = (
    "ts|tsx|js|jsx|mjs|cjs|py|json|md|yml|yaml|toml|sql|prisma|css|scss|html|sh|go|rs|java|kt|swift|ejs|"
    "conf|tf|rb|php|c|h|cpp|vue|svelte"
)
# path/with.ext  or  file.ext:12  or  file.ext:12-30   (not URLs, not versions)
_REF = re.compile(rf"(?<![\w/:.@-])((?:[\w.@\-]+/)*[\w.@\-]+\.(?:{EXTS}))(?::(\d+)(?:[-–](\d+))?)?(?![\w/])")
MAX_REFS = 40


@dataclass
class Problem:
    ref: str
    why: str


def extract_refs(text: str) -> list[tuple[str, int | None, int | None]]:
    seen, out = set(), []
    for m in _REF.finditer(re.sub(r"https?://\S+", "", text)):
        path, a, b = m.group(1), m.group(2), m.group(3)
        if "/" not in path and a is None:
            continue  # a bare name like `index.ts` is too ambiguous to judge
        key = (path, a, b)
        if key not in seen:
            seen.add(key)
            out.append((path, int(a) if a else None, int(b) if b else None))
    return out[:MAX_REFS]


async def _git_files(cwd: Path) -> list[str] | None:
    p = await asyncio.create_subprocess_exec(
        "git", "ls-files", "-co", "--exclude-standard", cwd=cwd,
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
    )
    out, _ = await p.communicate()
    return out.decode("utf-8", "replace").splitlines() if p.returncode == 0 else None


async def _tree(cwd: Path) -> list[str]:
    files = await _git_files(cwd)
    if files is not None:
        return files
    # started in a folder that holds repos (~/Devlopment with ~/Devlopment/tunyt inside): list each
    # repo one level down, so "apps/server/x.ts" still resolves instead of being called made up
    try:
        repos = [d for d in sorted(cwd.iterdir()) if (d / ".git").exists()]
    except OSError:
        return []
    lists = await asyncio.gather(*(_git_files(d) for d in repos))
    return [f"{d.name}/{f}" for d, fs in zip(repos, lists) for f in fs or []]


def _resolve(cwd: Path, ref: str, tree: list[str]) -> Path | None:
    direct = cwd / ref
    if direct.is_file():
        return direct
    # models cite paths relative to a sub-app (src/app.ts for apps/server/src/app.ts)
    hits = [t for t in tree if t == ref or t.endswith("/" + ref)]
    return cwd / hits[0] if hits else None


async def verify_references(text: str, cwd: Path) -> tuple[int, list[Problem]]:
    refs = extract_refs(text)
    if not refs:
        return 0, []
    tree = await _tree(cwd)
    problems: list[Problem] = []
    for path, a, b in refs:
        f = _resolve(cwd, path, tree)
        label = path + (f":{a}" if a else "") + (f"-{b}" if b else "")
        if f is None:
            problems.append(Problem(label, "no such file in the workspace"))
            continue
        if a is not None:
            try:
                with f.open("rb") as fh:
                    n = sum(1 for _ in fh)
            except OSError:
                continue
            if a > n or (b is not None and b > n + 1):
                problems.append(Problem(label, f"file has only {n} lines"))
    return len(refs), problems


def correction_prompt(problems: list[Problem]) -> str:
    rows = "\n".join(f"- {p.ref}: {p.why}" for p in problems[:15])
    return (
        "Automatic check: these references in your last answer do not exist:\n"
        f"{rows}\n\nRe-check each with your tools (grep/read_file) and correct it, or remove the claim. "
        "Do not guess a replacement. Reply with the corrected answer only."
    )


_EDIT_VERBS = re.compile(
    r"\b(fix|add|implement|change|update|refactor|rename|write|create|remove|delete|replace|make|move|extract|"
    r"split|convert|migrate|rewrite|patch|build|set up|wire)\b", re.I)
_READONLY = re.compile(r"\b(read[- ]only|do not (modify|change|edit)|don't (modify|change|edit)|without (modifying|changing)|"
                       r"no (changes|edits)|just (explain|tell|show|answer)|investigate|explain|what is|where is|why)\b", re.I)
_SHELLISH = {"bash", "sh", "shell", "zsh", "console", "sql", "text", "txt", "output", "log", "diff", "http", "curl"}


def wants_change(request: str) -> bool:
    """Does the request ask for a change to files? Read-only questions must not be nudged to write."""
    return bool(_EDIT_VERBS.search(request)) and not _READONLY.search(request)


def has_file_code(answer: str) -> bool:
    """A fenced block in a source language (not a command or output the user is meant to run or read)."""
    langs = re.findall(r"```([\w+#-]*)", answer)
    return any(lang.lower() not in _SHELLISH for lang in langs[::2] or langs)


# a review answer that argues with itself: findings the model dismissed in the same breath
_SECOND_THOUGHTS = re.compile(
    r"non-issue|not an? (real )?(bug|issue|problem|defect|regression)|no (defect|regression|issue here)|\bskip\b\.?|acceptable[;.,]|"
    r"actually (fine|ok|correct)|coincidence|no collision here|\bverified (correct|fine)|pre-existing behaviou?r|nothing to fix|"
    r"^\W*final answer\b",
    re.I | re.M,
)


def shows_second_thoughts(request: str, answer: str) -> bool:
    """A review that lists a finding and then talks itself out of it ("... actually a non-issue. Skip.") has
    put its scratch thinking in the answer. One such phrase is enough: a finding is a defect, so any "no defect" is a deleted item.
    Only review requests are checked."""
    return bool(re.search(r"\breview", request, re.I)) and _SECOND_THOUGHTS.search(answer) is not None


REWRITE_PROMPT = (
    "Your review lists findings you then dismissed (non-issue, acceptable, skip, verified fine). Rewrite it: delete every item "
    "you dismissed, keep only defects you can point to as `severity file:line  what is wrong -> fix`, at most 6, two lines each, "
    "no deliberation. If a real risk is left, such as a new awaited call on a request path whose failure now fails the whole "
    "response, state it. Reply with the rewritten review only."
)
