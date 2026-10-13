from __future__ import annotations
from enum import Enum
import os
from pathlib import Path
from typing import Any
from pydantic import BaseModel, Field, model_validator


DEFAULT_BASE_URL = "https://api.commandcode.ai/provider/v1"


class ModelConfig(BaseModel):
    name: str = "codex/gpt-5.6-luna"  # your ChatGPT/Codex login; override per project/profile or with -m
    temperature: float = Field(default=1, ge=0.0, le=2.0)
    context_window: int = 256_000


class ShellEnvironmentPolicy(BaseModel):
    ignore_default_excludes: bool = False
    exclude_patterns: list[str] = Field(
        default_factory=lambda: ["*KEY*", "*TOKEN*", "*SECRET*"]
    )
    set_vars: dict[str, str] = Field(default_factory=dict)


class MCPServerConfig(BaseModel):
    enabled: bool = True
    startup_timeout_sec: float = 10

    # stdio transport
    command: str | None = None
    args: list[str] = Field(default_factory=list)
    env: dict[str, str] = Field(default_factory=dict)
    cwd: Path | None = None

    # http/sse transport
    url: str | None = None

    @model_validator(mode="after")
    def validate_transport(self) -> MCPServerConfig:
        has_command = self.command is not None
        has_url = self.url is not None

        if not has_command and not has_url:
            raise ValueError(
                "MCP Server must have either 'command' (stdio) or 'url' (http/sse)"
            )

        if has_command and has_url:
            raise ValueError(
                "MCP Server cannot have both 'command' (stdio) and 'url' (http/sse)"
            )

        return self


class ApprovalPolicy(str, Enum):
    ON_REQUEST = "on-request"
    ON_FAILURE = "on-failure"
    AUTO = "auto"
    AUTO_EDIT = "auto-edit"
    NEVER = "never"
    YOLO = "yolo"


class HookTrigger(str, Enum):
    BEFORE_AGENT = "before_agent"
    AFTER_AGENT = "after_agent"
    BEFORE_TOOL = "before_tool"
    AFTER_TOOL = "after_tool"
    ON_ERROR = "on_error"


class HookConfig(BaseModel):
    name: str
    trigger: HookTrigger
    command: str | None = None  # python3 tests.py
    script: str | None = None  # *.sh
    timeout_sec: float = 30
    enabled: bool = True

    @model_validator(mode="after")
    def validate_hook(self) -> HookConfig:
        if not self.command and not self.script:
            raise ValueError("Hook must either have 'command' or 'script'")
        return self


class ModelPrice(BaseModel):
    """USD per 1M tokens. Left empty by default: prices are not invented here."""

    input: float
    output: float
    cached_input: float | None = None


class SubagentSpec(BaseModel):
    """A subagent a profile can define without touching code. `role` picks the
    model chain from [routing] (e.g. review -> an independent model)."""

    name: str
    description: str
    goal_prompt: str
    allowed_tools: list[str] | None = None
    max_turns: int = 20
    timeout_seconds: float = 600
    role: str = "default"


class VerifyRule(BaseModel):
    """Run `commands` when a changed file starts with one of `paths`. Commands
    run in `cwd` (relative to the repo root); {files} expands to the changed
    files under that rule, relative to `cwd`, shell-quoted."""

    paths: list[str]
    commands: list[str]
    cwd: str = "."
    # {tests} expands to spec files under `cwd` matching this glob that mention a changed
    # file's name in an import; a command that needs {tests} is skipped when none match
    tests_glob: str | None = None


class Config(BaseModel):
    model: ModelConfig = Field(default_factory=ModelConfig)
    cwd: Path = Field(default_factory=Path.cwd)
    shell_environment: ShellEnvironmentPolicy = Field(
        default_factory=ShellEnvironmentPolicy
    )
    hooks_enabled: bool = False
    hooks: list[HookConfig] = Field(default_factory=list)
    approval: ApprovalPolicy = ApprovalPolicy.ON_REQUEST
    max_turns: int = 100
    mcp_servers: dict[str, MCPServerConfig] = Field(default_factory=dict)

    # role -> ordered model ids. Roles in use: default (fallbacks after the main
    # model), explore, review, summarize. A missing role falls back to default.
    routing: dict[str, list[str]] = Field(default_factory=dict)
    trace: bool = True
    zdr: bool = False  # send x-cmd-zdr: 1 (zero data retention routing; fails if a model has no ZDR upstream)

    # Verification gate. After a run that changed files, these commands must
    # pass before the task counts as verified. Empty = auto-detect (pytest, go
    # test, cargo test); set explicitly for anything heavier.
    verify_commands: list[str] = Field(default_factory=list)
    verify_rules: list[VerifyRule] = Field(default_factory=list)

    # Repository rules enforced by the tool layer (not just asked of the model).
    # forbidden_paths are fnmatch globs on file names; forbidden_commands are
    # regexes tried against shell commands.
    forbidden_paths: list[str] = Field(
        default_factory=lambda: [".env", ".env.local", ".env.development", ".env.production", ".env.staging",
                                 ".env.test", ".env.*.local", "*.pem", "*.key", "id_rsa*", "id_ed25519*", "*.p12"]
    )
    forbidden_commands: list[str] = Field(default_factory=list)
    profile: str | None = None

    # Workspace guardrails. The session may not leave `cwd` (plus allowed_dirs).
    allowed_dirs: list[str] = Field(default_factory=list)  # extra roots the agent may use (--add-dir)
    sandbox: str = "auto"  # "auto": OS sandbox for shell commands when available; "off"
    sandbox_write_extra: list[str] = Field(default_factory=list)  # extra dirs shell commands may write
    allow_private_network: bool = False  # web_fetch may reach localhost / private ranges

    subagents: list[SubagentSpec] = Field(default_factory=list)
    disabled_skills: list[str] = Field(default_factory=list)  # e.g. ["ponytail"] to turn the built-in style policy off
    skill_dirs: list[str] = Field(default_factory=list)  # repo-relative dirs holding extra skill .md files
    model_role: str = "default"  # which routing chain this agent's own calls use
    auto_route: bool = True  # classify each request into the fast/default/strong chains when [routing] defines them
    model_pinned: bool = False  # set by -m or /model: the user chose, so never auto-route away from it
    max_parallel_agents: int = 4  # concurrent sub-agents / workers
    enable_parallel_tasks: bool = True  # expose the parallel_tasks tool (off inside workers and sub-agents)
    auto_scout: bool = True  # non-trivial requests get a cheap read-only investigator pass first (if the profile defines one)
    auto_review: bool = False  # independent reviewer pass over the real diff before finishing
    max_reviews: int = 1
    check_references: bool = True  # verify file:line citations in the final answer against the tree
    require_read_before_edit: bool = True  # editing a file the agent never read is refused
    auto_verify: bool = True
    max_repairs: int = 2
    verify_timeout_sec: int = 600

    max_total_tokens: int = 1_500_000  # processed tokens (incl. cached) per request; stops runaway runs
    tool_output_max_chars: int = 16_000  # per tool result sent to the model
    pricing: dict[str, ModelPrice] = Field(default_factory=dict)  # model id -> price, for cost estimates

    max_seconds: float = 1800  # wall-clock budget for one user request
    compact_at_tokens: int = 200_000  # compact earlier than the model window
    keep_recent_tokens: int = 20_000  # tail kept verbatim when compacting

    allowed_tools: list[str] | None = Field(
        None,
        description="If set, only these tools will be available to the agent",
    )

    developer_instructions: str | None = None
    user_instructions: str | None = None

    debug: bool = False
    ignored_project_keys: list[str] = Field(default_factory=list)  # project settings skipped until trusted

    @property
    def api_key(self) -> str | None:
        return os.environ.get("API_KEY")

    @property
    def base_url(self) -> str | None:
        return os.environ.get("BASE_URL") or DEFAULT_BASE_URL

    @property
    def model_name(self) -> str:
        return self.model.name

    @model_name.setter
    def model_name(self, value: str) -> None:
        self.model.name = value

    @property
    def temperature(self) -> float:
        return self.model.temperature

    @temperature.setter
    def temperature(self, value: float) -> None:
        self.model.temperature = value

    def validate(self) -> list[str]:
        errors: list[str] = []

        if not self.api_key and not self.model_name.startswith("codex/"):
            errors.append("No API key found. Set API_KEY environment variable")

        if not self.cwd.exists():
            errors.append(f"Working directory does not exist: {self.cwd}")

        return errors

    def to_dict(self) -> dict[str, Any]:
        return self.model_dump(mode="json")
