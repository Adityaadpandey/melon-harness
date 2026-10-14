"""Skills: curated playbooks the agent can load instead of rediscovering a
project's rules every run.

A skill is a markdown file with a small header:

    ---
    name: debugging
    description: one line shown in the index the model always sees
    triggers: bug, error, 500, stuck job      # words that auto-load it
    canonical: docs/skills/tunyt-debugging.md  # optional long source doc
    ---
    ...distilled playbook body...

The body is short enough to load on every relevant task. `canonical` points at
the full document in the repo; the agent pulls single sections of it on demand
(use_skill with `section`) so an 80KB runbook never lands in context whole."""
from __future__ import annotations

import re
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path

PROFILES_DIR = Path(__file__).resolve().parent.parent / "profiles"
BUILTIN_DIR = Path(__file__).resolve().parent.parent / "skills"  # ship with the harness, apply to every project
USER_DIRS = (Path.home() / ".pi" / "agent" / "skills",)  # shared with pi: <name>/SKILL.md folders
SECTION_CAP = 14_000
AUTO_LOAD_CAP = 9_000  # total chars of playbooks auto-injected per request


@dataclass
class Skill:
    name: str
    description: str
    body: str
    triggers: list[str] = field(default_factory=list)
    canonical: str | None = None
    path: Path | None = None
    always: bool = False  # inlined into the system prompt instead of waiting to be triggered

    def score(self, text: str) -> int:
        low = text.lower()
        return sum(1 for t in self.triggers if re.search(rf"(?<!\w){re.escape(t.lower())}", low))


def parse_skill(text: str, path: Path | None = None) -> Skill | None:
    m = re.match(r"---\n(.*?)\n---\n(.*)", text, re.S)
    if not m:
        return None
    meta = {}
    for line in m.group(1).splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            meta[k.strip()] = v.split("  #")[0].strip()
    if "name" not in meta:
        return None
    return Skill(
        name=meta["name"],
        description=meta.get("description", ""),
        body=m.group(2).strip(),
        triggers=[t.strip() for t in meta.get("triggers", "").split(",") if t.strip()],
        canonical=meta.get("canonical") or None,
        path=path,
        always=meta.get("always", "").lower() == "true",
    )


@lru_cache(maxsize=16)
def _load(profile: str | None, cwd: str, skill_dirs: tuple[str, ...]) -> tuple[Skill, ...]:
    dirs: list[Path] = [BUILTIN_DIR, *USER_DIRS]
    if profile:
        dirs.append(PROFILES_DIR / profile / "skills")
    root = Path(cwd)
    dirs += [root / ".ai-agent" / "skills", *(root / d for d in skill_dirs)]
    found: dict[str, Skill] = {}
    for d in dirs:
        if d.is_dir():
            # flat files (skills/debugging.md) and pi-style folders (skills/review-pr/SKILL.md)
            for f in sorted([*d.glob("*.md"), *d.glob("*/SKILL.md")]):
                sk = parse_skill(f.read_text(encoding="utf-8"), f)
                if sk:
                    if not sk.triggers and f.name == "SKILL.md" and not sk.always:
                        sk.triggers = [sk.name, sk.name.replace("-", " ")]  # pi skills carry no trigger words, the name is the hint
                    found[sk.name] = sk  # later dirs (repo-local) override profile ones
    if profile:
        # a profile's own playbook (e.g. tunyt-debugging) replaces the generic one it specialises
        for name in list(found):
            if f"{profile}-{name}" in found:
                del found[name]
    return tuple(found.values())


def load_skills(config) -> list[Skill]:
    off = set(config.disabled_skills)
    return [s for s in _load(config.profile, str(config.cwd), tuple(config.skill_dirs)) if s.name not in off]


def match_skills(skills: list[Skill], request: str, limit: int = 2) -> list[Skill]:
    """Deterministic keyword routing: no model call, so it is free, instant and testable."""
    scored = sorted(((s.score(request), s) for s in skills if not s.always), key=lambda x: -x[0])
    picked, used = [], 0
    for score, s in scored:
        if score < 1 or len(picked) >= limit:
            break
        if used + len(s.body) > AUTO_LOAD_CAP and picked:
            continue
        picked.append(s)
        used += len(s.body)
    return picked


def headings(text: str) -> list[tuple[int, int, str]]:
    """(line number, level, title) for every markdown heading outside code fences."""
    out, fenced = [], False
    for i, line in enumerate(text.splitlines(), 1):
        if line.startswith("```"):
            fenced = not fenced
        elif not fenced and (m := re.match(r"(#{1,4})\s+(.*)", line)):
            out.append((i, len(m.group(1)), m.group(2).strip()))
    return out


def extract_section(text: str, query: str) -> str | None:
    lines = text.splitlines()
    hs = headings(text)
    q = query.lower()
    for idx, (ln, level, title) in enumerate(hs):
        if q in title.lower():
            end = len(lines)
            for ln2, level2, _ in hs[idx + 1:]:
                if level2 <= level:
                    end = ln2 - 1
                    break
            return "\n".join(lines[ln - 1:end])
    return None


def find_in_repo(cwd: Path, rel: str) -> Path | None:
    """Canonical docs are repo-root relative; the agent may be running in a subfolder."""
    for d in [cwd.resolve(), *cwd.resolve().parents]:
        if (d / rel).is_file():
            return d / rel
    return None
