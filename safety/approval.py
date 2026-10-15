from dataclasses import dataclass
from enum import Enum
from pathlib import Path
import re
import time
import shlex
from typing import Any, Awaitable, Callable
from config.config import ApprovalPolicy
from tools.base import ToolConfirmation


class ApprovalDecision(str, Enum):
    APPROVED = "approved"
    REJECTED = "rejected"
    NEEDS_CONFIRMATION = "needs_confirmation"


@dataclass
class ApprovalContext:

    tool_name: str
    params: dict[str, Any]
    is_mutating: bool
    affected_paths: list[Path]
    command: str | None = None
    is_dangerous: bool = False
    tool_kind: str | None = None  # "write" for file edits, "shell", ...


@dataclass
class Decision:
    """What the user chose at a prompt. `always` = stop asking for this kind of
    action for the rest of the session; `feedback` = what to do instead."""

    approved: bool
    always: bool = False
    feedback: str | None = None


# commands where the first word alone is too broad to remember ("git status" vs "git push")
_TWO_WORD = {"git", "npm", "pnpm", "yarn", "docker", "cargo", "make", "npx", "uv", "pip", "go"}


def command_key(command: str) -> str:
    words = command.strip().split()
    if not words:
        return ""
    return " ".join(words[:2]) if words[0] in _TWO_WORD and len(words) > 1 else words[0]


DANGEROUS_PATTERNS = [
    # File system destruction
    r"rm\s+(-rf?|--recursive)\s+[/~]",
    r"rm\s+-rf?\s+\*",
    r"rmdir\s+[/~]",
    # Git history / uncommitted work
    r"rm\s+(-\S+\s+)*\S*\.git(/|\s|$)",
    r"git\s+reset\s+--hard",
    r"git\s+clean\s+-\S*f",
    r"git\s+push\s+.*(--force|\s-f(\s|$))",
    r"git\s+(checkout|restore)\s+(--\s+)?\.(\s|$)",
    r"git\s+branch\s+(?-i:-D)",  # -d (safe delete) is fine, -D is not
    # Disk operations
    r"dd\s+if=",
    r"mkfs",
    r"fdisk",
    r"parted",
    # System control
    r"shutdown",
    r"reboot",
    r"halt",
    r"poweroff",
    r"init\s+[06]",
    # Permission changes on root
    r"chmod\s+(-R\s+)?777\s+[/~]",
    r"chown\s+-R\s+.*\s+[/~]",
    # Network exposure
    r"nc\s+-l",
    r"netcat\s+-l",
    # Code execution from network
    r"curl\s+.*\|\s*(bash|sh)",
    r"wget\s+.*\|\s*(bash|sh)",
    # Fork bomb
    r":\(\)\s*\{\s*:\|:&\s*\}\s*;",
]

# Read-only commands that may run without asking. A command is safe only when EVERY
# piece of it is on this list: "ls; rm x", "cat a > b" or "ls $(rm x)" are not, even
# though they start with ls/cat. Matching just the first word is how an allowlist leaks.
# `name: reject` lists flags that turn an otherwise read-only command into a writer.
_PLAIN_SAFE = {
    "ls": set(), "pwd": set(), "echo": set(), "cat": set(), "head": set(), "wc": set(),
    "tail": {"-f", "-F", "--follow"},  # follow never ends
    "which": set(), "whereis": set(), "file": set(), "stat": set(), "basename": set(), "dirname": set(),
    "realpath": set(), "grep": set(), "cut": set(), "uniq": set(), "tr": set(), "diff": set(), "comm": set(),
    "sort": {"-o", "--output"}, "date": {"-s", "--set"}, "uname": set(), "whoami": set(), "id": set(),
    "groups": set(), "uptime": set(), "ps": set(), "pgrep": set(), "cd": set(), "true": set(),
    "find": {"-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fprintf", "-fls"},
    "rg": {"--pre", "--pre-glob"},  # --pre runs a program on every file
    "fd": {"-x", "--exec", "-X", "--exec-batch"}, "tree": {"-o"}, "nl": set(),
}
# env, printenv and set are left out on purpose: they print every secret in the environment.
_GIT_READ = {"status", "log", "diff", "show", "blame", "rev-parse", "ls-files", "shortlog", "describe", "grep"}
_GIT_LISTING = {"branch": {"-a", "-r", "-v", "-vv", "--list", "--show-current", "--all"},
                "tag": {"-l", "--list"}, "remote": {"-v"}}
_GIT_BAD_FLAGS = ("--output", "--ext-diff", "--textconv", "--open-files-in-pager", "-c", "-C", "--exec-path", "--git-dir", "--work-tree")
_PKG_READ = {"npm": {"list", "ls", "outdated"}, "yarn": {"list", "outdated"}, "pnpm": {"list", "ls", "outdated"},
             "pip": {"list", "show", "freeze"}, "cargo": {"tree"}}
# sed only as a line printer: `sed -n '10,40p' a.py`. Its w and e commands write files and run programs.
_SED_PRINT = re.compile(r"(\d+|\$)(,(\d+|\$))?p")
# output thrown away or merged into the pipe writes nothing, so `grep -rn x . 2>/dev/null` stays a read
_HARMLESS_REDIRECT = re.compile(r"(?:[12&]?>\s*/dev/null|2>&1)(?=[\s|;&]|$)")


def split_pipeline(command: str) -> list[list[str]] | None:
    """The commands of `a | b && c; d` as word lists, or None when the line does more than run
    commands one after another: a redirect into a file, background (&), substitution ($( ), ` `, ${ }),
    a subshell, an unquoted backslash or a second line. Quotes are respected, so
    `grep -E "foo|bar" src` is one command, not a pipe. A refusal here only means "ask"."""
    if re.search(r"`|\$[({]", command):
        return None
    parts, cur, quote, i = [], "", None, 0
    while i < len(command):
        c = command[i]
        if quote:
            if c == "\\" and quote == '"':  # inside double quotes a backslash only escapes the next character
                cur, i = cur + command[i:i + 2], i + 2
                continue
            quote = None if c == quote else quote
        elif c in "'\"":
            quote = c
        elif m := _HARMLESS_REDIRECT.match(command, i):
            i = m.end()
            continue
        elif op := next((o for o in ("&&", "||", "|", ";") if command.startswith(o, i)), None):
            parts.append(cur)
            cur, i = "", i + len(op)
            continue
        elif c in "<>&()\\\n":
            return None
        cur, i = cur + c, i + 1
    if quote:
        return None
    out = []
    for part in parts + [cur]:
        try:
            words = shlex.split(part)
        except ValueError:
            return None
        if not words:
            return None
        out.append(words)
    return out


def _has_flag(args: list[str], flags: set[str]) -> bool:
    """Catches every spelling of a flag: `-o`, `--output=x` and `-ro` (short flags bundled together)."""
    short = {f[1] for f in flags if len(f) == 2 and f[1] != "-"}
    return any(
        a in flags or a.split("=", 1)[0] in flags or (a[:1] == "-" and a[1:2] != "-" and short & set(a[1:]))
        for a in args
    )


def _safe_words(words: list[str]) -> bool:
    name, args = words[0], words[1:]
    if name in _PLAIN_SAFE:
        return not _has_flag(args, _PLAIN_SAFE[name])
    if name == "sed":
        return len(args) >= 2 and args[0] == "-n" and bool(_SED_PRINT.fullmatch(args[1])) and not any(a.startswith("-") for a in args[2:])
    if name == "git":
        if not args or any(a.startswith(_GIT_BAD_FLAGS) for a in args):
            return False
        sub, rest = args[0], args[1:]
        if sub in _GIT_READ:
            return True
        if sub == "stash":  # list and show only read; push, pop, drop and clear change the tree
            return bool(rest) and rest[0] in ("list", "show")
        return sub in _GIT_LISTING and set(rest) <= _GIT_LISTING[sub]
    if name in _PKG_READ:
        return bool(args) and args[0] in _PKG_READ[name]
    return False


def is_safe_command(command: str) -> bool:
    pipeline = split_pipeline(command.strip())
    return bool(pipeline) and all(_safe_words(w) for w in pipeline)


def is_dangerous_command(command: str) -> bool:
    for pattern in DANGEROUS_PATTERNS:
        if re.search(pattern, command, re.IGNORECASE):
            return True

    return False


class ApprovalManager:
    def __init__(
        self,
        approval_policy: ApprovalPolicy,
        cwd: Path,
        confirmation_callback: Callable[[ToolConfirmation], bool] | None = None,
        verify_commands: list[str] | None = None,
    ) -> None:
        self.approval_policy = approval_policy
        self.cwd = Path(cwd).resolve()  # symlinked tmp dirs (/var -> /private/var) must still match
        self.confirmation_callback = confirmation_callback
        self.always_commands: set[str] = set()  # "don't ask again" choices made this session
        self.verify_commands = {c.strip() for c in verify_commands or [] if c.strip()}
        self.waited = 0.0  # total seconds spent waiting for the user's answer; the agent's clock stops meanwhile

    def _is_verify_command(self, command: str) -> bool:
        """The repo's own check, e.g. `pnpm check-types`, optionally followed by `2>&1` and a safe
        pipe like `| tail -20`. The commands come from trusted config, so they run without asking.
        ponytail: exact match only; `cd dir && pnpm test` still asks."""
        first, _, rest = command.strip().partition("|")
        first = first.strip().removesuffix("2>&1").strip()
        if first not in self.verify_commands:
            return False
        return not rest or is_safe_command(rest)

    def _always_allowed(self, command: str) -> bool:
        """A remembered "don't ask again" covers the command the user saw, not whatever is chained
        after it: `npm run build; rm -rf x` must still ask. Each piped part has to be remembered."""
        pipeline = split_pipeline(command.strip())
        return bool(pipeline) and all(command_key(" ".join(w)) in self.always_commands for w in pipeline)

    def _assess_command_safety(self, command: str) -> ApprovalDecision:
        if self.approval_policy == ApprovalPolicy.YOLO:
            return ApprovalDecision.APPROVED

        if is_dangerous_command(command):
            return ApprovalDecision.REJECTED

        if self._always_allowed(command) or self._is_verify_command(command):
            return ApprovalDecision.APPROVED

        if self.approval_policy == ApprovalPolicy.NEVER:
            if is_safe_command(command):
                return ApprovalDecision.APPROVED
            return ApprovalDecision.REJECTED

        if self.approval_policy in {ApprovalPolicy.AUTO, ApprovalPolicy.ON_FAILURE}:
            return ApprovalDecision.APPROVED

        if self.approval_policy == ApprovalPolicy.AUTO_EDIT:
            if is_safe_command(command):
                return ApprovalDecision.APPROVED

            return ApprovalDecision.NEEDS_CONFIRMATION

        if is_safe_command(command):
            return ApprovalDecision.APPROVED

        return ApprovalDecision.NEEDS_CONFIRMATION

    async def check_approval(self, context: ApprovalContext) -> ApprovalDecision:
        if not context.is_mutating:
            return ApprovalDecision.APPROVED

        if context.command:
            # a command that needs confirmation must stay that way; falling
            # through to the path checks below used to approve it silently
            return self._assess_command_safety(context.command)

        policy = self.approval_policy
        if policy == ApprovalPolicy.YOLO:
            return ApprovalDecision.APPROVED

        outside = any(not Path(p).resolve().is_relative_to(self.cwd) for p in context.affected_paths)
        if outside:
            return ApprovalDecision.REJECTED if policy == ApprovalPolicy.NEVER else ApprovalDecision.NEEDS_CONFIRMATION

        if context.tool_kind == "write":
            # file edits: ask by default; "auto-edit"/"auto" pre-approve edits inside the workspace
            if policy in {ApprovalPolicy.AUTO, ApprovalPolicy.AUTO_EDIT, ApprovalPolicy.ON_FAILURE}:
                return ApprovalDecision.APPROVED
            if policy == ApprovalPolicy.NEVER:
                return ApprovalDecision.REJECTED
            return ApprovalDecision.NEEDS_CONFIRMATION

        if context.is_dangerous and policy not in {ApprovalPolicy.AUTO, ApprovalPolicy.AUTO_EDIT}:
            return ApprovalDecision.NEEDS_CONFIRMATION

        return ApprovalDecision.APPROVED

    async def request_confirmation(self, confirmation: ToolConfirmation) -> Decision:
        """Ask the user. The callback may be sync or async and may return a bool
        (old style) or a Decision."""
        if not self.confirmation_callback:
            # nobody to ask (single-run mode): fail closed instead of approving silently
            return Decision(
                False,
                feedback="This needs approval but the run is non-interactive. Re-run with "
                "--approval auto-edit (allow edits) or --approval auto (edits and commands).",
            )
        started = time.monotonic()
        try:
            result = self.confirmation_callback(confirmation)
            if hasattr(result, "__await__"):
                result = await result
        finally:
            self.waited += time.monotonic() - started
        return result if isinstance(result, Decision) else Decision(bool(result))

    def remember(self, confirmation: ToolConfirmation) -> None:
        """Apply a 'don't ask again' choice for the rest of the session."""
        if confirmation.command:
            self.always_commands.add(command_key(confirmation.command))
        else:
            self.approval_policy = ApprovalPolicy.AUTO_EDIT  # edits only; commands still ask
