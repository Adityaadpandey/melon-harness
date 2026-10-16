"""Allow / deny / ask rules in Claude Code's notation, so a project can say what Mew may do without a prompt.

    Bash(git diff:*)     any command starting with `git diff`
    Bash(npm run build)  exactly that command
    Edit(src/**)         edits (and new files) anywhere under src/
    Read(.env*)          reading, grepping or listing anything named .env...
    WebFetch(domain:docs.python.org)
    Bash                 the whole tool (no specifier)

Example `.ai-agent/permissions.json`:  {"allow": ["Bash(pnpm test:*)"], "deny": ["Read(.env*)", "Bash(rm:*)"], "ask": ["Bash(git push:*)"]}

Deny wins over ask, ask over allow. An allow never lifts the built-in refusals (plan mode, dangerous commands)."""
from __future__ import annotations

import fnmatch
import json
import re
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlparse

from safety.approval import split_pipeline

# the names people know from Claude Code -> the tool names Mew registers
ALIASES = {
    "bash": {"shell"},
    "edit": {"edit", "write_file"},
    "write": {"write_file"},
    "read": {"read_file", "grep", "glob", "list_dir", "search", "outline"},
    "webfetch": {"web_fetch"},
    "websearch": {"web_search"},
}
KINDS = ("deny", "ask", "allow")
RULE = re.compile(r"^\s*([\w.\-]+)\s*(?:\((.*)\))?\s*$", re.S)


def _glob(spec: str) -> re.Pattern:
    """Gitignore-flavoured: `src/**` everything under src, `*.key` that name at any depth, `docs/*.md` one level only."""
    spec = spec.removeprefix("./")
    anchored = spec.startswith("/") or "/" in spec.rstrip("/")
    spec = spec.lstrip("/")
    body, i = "", 0
    while i < len(spec):
        if spec.startswith("**/", i):
            body += "(?:.*/)?"; i += 3
        elif spec.startswith("**", i):
            body += ".*"; i += 2
        elif spec[i] == "*":
            body += "[^/]*"; i += 1
        elif spec[i] == "?":
            body += "[^/]"; i += 1
        else:
            body += re.escape(spec[i]); i += 1
    if spec.endswith("/"):
        body += ".*"  # a folder name covers what is inside it
    return re.compile(body if anchored else f"(?:.*/)?{body}(?:/.*)?")


@dataclass(frozen=True)
class Rule:
    kind: str
    tool: str
    spec: str | None
    text: str
    source: str = ""

    def covers_tool(self, name: str) -> bool:
        return name in ALIASES.get(self.tool.lower(), {self.tool}) or name == self.tool

    def _path_matches(self, target: str, cwd: Path) -> bool:
        try:
            rel = Path(target).resolve().relative_to(cwd.resolve()).as_posix()
        except (ValueError, OSError):
            rel = Path(target).as_posix()
        return bool(_glob(self.spec or "*").fullmatch(rel))

    def _command_matches(self, command: str) -> bool:
        spec = (self.spec or "").strip()
        if spec.endswith(":*"):
            head = spec[:-2].strip()
            return command == head or command.startswith(head + " ")
        if "*" in spec:
            return fnmatch.fnmatch(command, spec)
        return command == spec

    def matches(self, name: str, params: dict, cwd: Path) -> bool:
        if not self.covers_tool(name):
            return False
        if self.spec is None:
            return True
        if name == "shell":
            command = str(params.get("command", "")).strip()
            parts = split_pipeline(command)
            if parts is None:  # a line that does more than chain commands: judge the whole text
                return self._command_matches(command) or (self.kind != "allow" and self.spec.removesuffix(":*") in command)
            joined = [" ".join(w) for w in parts]
            # deny / ask fire when ANY piece matches; allow only when EVERY piece does (`ls && rm x` is not "allowed" by Bash(ls))
            return all(self._command_matches(c) for c in joined) if self.kind == "allow" else any(self._command_matches(c) for c in joined)
        if name == "web_fetch":
            host = urlparse(str(params.get("url", ""))).hostname or ""
            want = self.spec.removeprefix("domain:")
            return host == want or host.endswith("." + want)
        target = params.get("path")
        if isinstance(target, str):
            return self._path_matches(target, cwd)
        return False


def parse_rule(text: str, kind: str, source: str = "") -> Rule | None:
    m = RULE.match(text or "")
    if not m or kind not in KINDS:
        return None
    return Rule(kind, m.group(1), (m.group(2) or "").strip() or None, text.strip(), source)


class Rules:
    def __init__(self, rules: list[Rule] | None = None, cwd: Path | None = None):
        self.rules = rules or []
        self.cwd = Path(cwd or ".")

    def decide(self, name: str, params: dict) -> Rule | None:
        """The rule that applies to this call (deny first, then ask, then allow), or None."""
        for kind in KINDS:
            for r in self.rules:
                if r.kind == kind and r.matches(name, params, self.cwd):
                    return r
        return None


def _read(path: Path) -> dict:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return data.get("permissions", data) if isinstance(data, dict) else {}


def files(cwd: Path) -> list[Path]:
    """Where rules come from, loosest first. Claude Code's per-project settings are honoured so the two agents agree;
    its global settings are not (they were written for a different tool's habits)."""
    home = Path.home()
    return [
        home / ".ai-agent" / "permissions.json",
        cwd / ".claude" / "settings.json",
        cwd / ".claude" / "settings.local.json",
        cwd / ".ai-agent" / "permissions.json",
    ]


def load_rules(cwd: Path) -> Rules:
    out: list[Rule] = []
    for f in files(cwd):
        block = _read(f)
        for kind in KINDS:
            for text in block.get(kind) or []:
                if isinstance(text, str) and (r := parse_rule(text, kind, str(f))):
                    out.append(r)
    return Rules(out, cwd)


def save_rule(cwd: Path, kind: str, text: str) -> Path:
    """Append to the project's own file; other keys in it are left as they are."""
    path = cwd / ".ai-agent" / "permissions.json"
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        data = {}
    if not isinstance(data, dict):
        data = {}
    bucket = data.setdefault(kind, [])
    if text not in bucket:
        bucket.append(text)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return path


def drop_rule(cwd: Path, text: str) -> bool:
    path = cwd / ".ai-agent" / "permissions.json"
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    hit = False
    for kind in KINDS:
        if text in (data.get(kind) or []):
            data[kind].remove(text)
            hit = True
    if hit:
        path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return hit
