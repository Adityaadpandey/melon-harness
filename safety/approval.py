from dataclasses import dataclass
from enum import Enum
from pathlib import Path
import re
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

# Patterns for safe commands (can be auto-approved)
SAFE_PATTERNS = [
    # Information commands
    r"^(ls|dir|pwd|cd|echo|cat|head|tail|less|more|wc)(\s|$)",
    r"^(find|locate|which|whereis|file|stat)(\s|$)",
    # Development tools (read-only)
    r"^git\s+(status|log|diff|show|branch|remote|tag)(\s|$)",
    r"^(npm|yarn|pnpm)\s+(list|ls|outdated)(\s|$)",
    r"^pip\s+(list|show|freeze)(\s|$)",
    r"^cargo\s+(tree|search)(\s|$)",
    # Text processing (usually safe)
    r"^(grep|awk|sed|cut|sort|uniq|tr|diff|comm)(\s|$)",
    # System info
    r"^(date|cal|uptime|whoami|id|groups|hostname|uname)(\s|$)",
    r"^(env|printenv|set)$",
    # Process info
    r"^(ps|top|htop|pgrep)(\s|$)",
]


def is_dangerous_command(command: str) -> bool:
    for pattern in DANGEROUS_PATTERNS:
        if re.search(pattern, command, re.IGNORECASE):
            return True

    return False


def is_safe_command(command: str) -> bool:
    for pattern in SAFE_PATTERNS:
        if re.search(pattern, command, re.IGNORECASE):
            return True

    return False


class ApprovalManager:
    def __init__(
        self,
        approval_policy: ApprovalPolicy,
        cwd: Path,
        confirmation_callback: Callable[[ToolConfirmation], bool] | None = None,
    ) -> None:
        self.approval_policy = approval_policy
        self.cwd = Path(cwd).resolve()  # symlinked tmp dirs (/var -> /private/var) must still match
        self.confirmation_callback = confirmation_callback
        self.always_commands: set[str] = set()  # "don't ask again" choices made this session

    def _assess_command_safety(self, command: str) -> ApprovalDecision:
        if self.approval_policy == ApprovalPolicy.YOLO:
            return ApprovalDecision.APPROVED

        if is_dangerous_command(command):
            return ApprovalDecision.REJECTED

        if command_key(command) in self.always_commands:
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
        result = self.confirmation_callback(confirmation)
        if hasattr(result, "__await__"):
            result = await result
        return result if isinstance(result, Decision) else Decision(bool(result))

    def remember(self, confirmation: ToolConfirmation) -> None:
        """Apply a 'don't ask again' choice for the rest of the session."""
        if confirmation.command:
            self.always_commands.add(command_key(confirmation.command))
        else:
            self.approval_policy = ApprovalPolicy.AUTO_EDIT  # edits only; commands still ask
