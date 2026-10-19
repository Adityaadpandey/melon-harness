import asyncio
import re
import weakref
from pathlib import Path
from typing import Any
from config.config import Config
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from dataclasses import dataclass
from pydantic import BaseModel, Field


class SubagentParams(BaseModel):
    goal: str = Field(
        ..., description="The specific task or goal for the subagent to accomplish"
    )


@dataclass
class SubagentDefinition:
    name: str
    description: str
    goal_prompt: str
    allowed_tools: list[str] | None = None
    max_turns: int = 20
    timeout_seconds: float = 600
    role: str = "default"


MAX_REPORT_CHARS = 6000  # a sub-agent's answer lands in the parent's context for the rest of the run
READONLY_TOOLS = {"read_file", "grep", "search", "outline", "git", "glob", "list_dir", "use_skill", "graph", "web_search", "web_fetch"}
_SEMAPHORES: "weakref.WeakKeyDictionary[asyncio.AbstractEventLoop, dict[int, asyncio.Semaphore]]" = weakref.WeakKeyDictionary()  # keyed by the loop itself: id() of a dead loop can be reused


def _agent_slots(limit: int) -> asyncio.Semaphore:
    """One cap shared by every sub-agent tool in this event loop."""
    per_loop = _SEMAPHORES.setdefault(asyncio.get_running_loop(), {})
    return per_loop.setdefault(max(1, limit), asyncio.Semaphore(max(1, limit)))


class SubagentTool(Tool):
    def __init__(self, config: Config, definition: SubagentDefinition):
        super().__init__(config)
        self.definition = definition
        # A sub-agent that can only read cannot conflict with anything, so several of
        # them run concurrently and need no approval. One that can run commands is serialized.
        tools = definition.allowed_tools
        self.readonly = bool(tools) and set(tools) <= READONLY_TOOLS
        self.kind = ToolKind.READ if self.readonly else ToolKind.SHELL

    @property
    def name(self) -> str:
        return f"subagent_{self.definition.name}"

    @property
    def description(self) -> str:
        return self.definition.description

    schema = SubagentParams

    def is_mutating(self, params: dict[str, Any]) -> bool:
        return not self.readonly

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        from agent.agent import Agent
        from agent.events import AgentEventType

        params = SubagentParams(**invocation.params)
        if not params.goal:
            return ToolResult.error_result("No goal specified for sub-agent")

        config_dict = self.config.to_dict()
        config_dict["max_turns"] = self.definition.max_turns
        config_dict["model_role"] = self.definition.role
        config_dict["auto_review"] = False
        config_dict["subagents"] = []  # no recursive delegation
        config_dict["enable_parallel_tasks"] = False
        config_dict["mcp_servers"] = {}  # starting every MCP server per sub-agent costs seconds and the task is local
        config_dict["auto_verify"] = False  # the parent run verifies the final tree
        config_dict["verify_commands"] = []
        if self.definition.allowed_tools:
            config_dict["allowed_tools"] = self.definition.allowed_tools

        subagent_config = Config(**config_dict)

        prompt = f"""You are a specialized sub-agent with a specific task to complete.

        {self.definition.goal_prompt}

        YOUR TASK:
        {params.goal}

        IMPORTANT:
        - Focus only on completing the specified task
        - Do not engage in unrelated actions
        - Once you have completed the task or have the answer, provide your final response
        - Be concise and direct in your output
        """

        tool_calls = []
        final_response = None
        error = None
        terminate_response = "goal"
        tokens = 0

        def report(info: dict) -> None:
            if invocation.progress:
                invocation.progress(invocation.call_id, {"agent": self.definition.name, **info})

        try:
            async with _agent_slots(self.config.max_parallel_agents), Agent(subagent_config) as agent:
                report({"kind": "start", "goal": params.goal})

                async def drive() -> None:
                    nonlocal final_response, error, terminate_response, tokens
                    async for event in agent.run(prompt):
                        tokens = agent._spent()
                        if event.type == AgentEventType.TOOL_CALL_START:
                            tool_calls.append(event.data.get("name"))
                            report({"kind": "tool", "name": event.data.get("name"), "args": event.data.get("arguments", {}), "tokens": tokens})
                        elif event.type == AgentEventType.TEXT_COMPLETE:
                            final_response = event.data.get("content")
                        elif event.type == AgentEventType.AGENT_END:
                            if final_response is None:
                                final_response = event.data.get("response")
                        elif event.type == AgentEventType.AGENT_ERROR:
                            terminate_response = "error"
                            error = event.data.get("error", "Unknown")
                            final_response = f"Sub-agent error: {error}"
                            return

                # wait_for cuts a hung model call or tool too; checking the clock between events could not
                try:
                    await asyncio.wait_for(drive(), self.definition.timeout_seconds)
                except asyncio.TimeoutError:
                    terminate_response = "timeout"
                    final_response = final_response or f"Sub-agent timed out after {self.definition.timeout_seconds:.0f}s"
        except Exception as e:
            terminate_response = "error"
            error = f"{type(e).__name__}: {e}"  # some exceptions (timeouts) have an empty message
            final_response = f"Sub-agent failed: {error}"

        report({"kind": "done", "ok": not error, "tokens": tokens})
        try:
            from config.config import HookTrigger
            from hooks.hook_system import HookSystem

            await HookSystem(self.config).fire(HookTrigger.SUBAGENT_STOP, agent=self.definition.name, ok=str(not error).lower())
        except Exception:
            pass  # a broken hook must not lose the sub-agent's answer
        if final_response and not error:
            from utils.search import check_paths

            # off the loop: listing a big repo for same-name files takes ~2.5s on tunyt
            if notes := await asyncio.to_thread(check_paths, Path(invocation.cwd), final_response, self.config.forbidden_paths):
                final_response = f"{notes}\n\n{final_response}"  # first, so a caller that cuts the report keeps them
        if final_response and len(final_response) > MAX_REPORT_CHARS:
            final_response = final_response[:MAX_REPORT_CHARS] + "\n... [report cut; ask for a narrower answer]"
        result = f"""Sub-agent '{self.definition.name}' completed. 
        Termination: {terminate_response}
        Tools called: {', '.join(tool_calls) if tool_calls else 'None'}

        Result:
        {final_response or 'No response'}
        """

        if error:
            # the reason goes first: UIs show only the top lines of a result
            return ToolResult.error_result(f"Sub-agent '{self.definition.name}' FAILED: {error}\n" + result)

        return ToolResult.success_result(result)


CODEBASE_INVESTIGATOR = SubagentDefinition(
    name="codebase_investigator",
    description="Read-only search agent for questions that need many files: where something lives, who calls it, how a flow works. Returns the answer with file:line evidence, not file dumps. Launch several in one turn for independent questions",
    goal_prompt="""You are a codebase search specialist. Answer the question with evidence and keep it short.
Start broad (graph, grep, glob, outline), then read only the lines that settle it.
Report file:line for every claim, and say what you could not find. Do NOT modify any files.""",
    allowed_tools=["read_file", "grep", "glob", "list_dir", "search", "outline", "graph", "git"],
    max_turns=15,
    role="explore",
)

PLANNER = SubagentDefinition(
    name="planner",
    description="Read-only planning agent: given a feature or change, studies the code and returns an implementation plan (files, steps, risks, how to verify). It never edits",
    goal_prompt="""You are a software architect. Study how the repo already does the nearest thing, then return a plan:
1) the goal in one line, 2) the files to change and what changes in each, in order, 3) risks and what could break (callers, migrations, permissions),
4) how to verify. Prefer the smallest plan that works. Do NOT modify any files.""",
    allowed_tools=["read_file", "grep", "glob", "list_dir", "search", "outline", "graph", "git"],
    max_turns=15,
)

CODE_REVIEWER = SubagentDefinition(
    name="code_reviewer",
    description="Reviews code changes and provides feedback on quality, bugs, and improvements",
    goal_prompt="""You are a code review specialist.
Your job is to review code and provide constructive feedback.
Look for bugs, code smells, security issues, and improvement opportunities.
Use read_file, list_dir and grep to examine the code.
Do NOT modify any files.""",
    allowed_tools=["read_file", "grep", "list_dir"],
    max_turns=10,
    timeout_seconds=300,
)

GENERAL = SubagentDefinition(
    name="general",
    description="General-purpose agent for a self-contained job in its own context: an independent second opinion on a commit or diff, multi-step research, a bounded fix in one area. Give it the full goal and what done looks like; it cannot ask you questions",
    goal_prompt="""You are a capable engineer handed one self-contained job. Do it end to end with the tools you have,
check your work, and report what you found or changed with file:line evidence. If the job is a review, report only
problems you verified. If you cannot finish, say what blocked you.""",
    max_turns=25,
)

# Claude Code's tool names, so an agent file written for it works here unchanged
TOOL_NAMES = {
    "read": "read_file", "grep": "grep", "glob": "glob", "ls": "list_dir", "bash": "shell", "edit": "edit",
    "multiedit": "edit", "write": "write_file", "webfetch": "web_fetch", "websearch": "web_search",
}


def agent_dirs(cwd) -> list[Path]:
    home = Path.home()
    return [home / ".claude" / "agents", home / ".ai-agent" / "agents", Path(cwd) / ".claude" / "agents", Path(cwd) / ".ai-agent" / "agents"]


def parse_agent_file(text: str) -> SubagentDefinition | None:
    """A markdown file like Claude Code's `.claude/agents/reviewer.md`:

        ---
        name: db-reviewer
        description: Reviews SQL and migrations. Use after schema changes.
        tools: Read, Grep, Bash       # left out = every tool
        model: haiku                  # haiku -> the fast model, anything else -> the default
        ---
        You review migrations for locks and data loss...
    """
    m = re.match(r"---\n(.*?)\n---\n?(.*)", text, re.S)
    if not m:
        return None
    meta = {}
    for line in m.group(1).splitlines():
        if ":" in line and not line[:1].isspace():
            k, v = line.split(":", 1)
            meta[k.strip().lower()] = v.split("  #")[0].strip().strip("'\"")
    name = re.sub(r"[^a-z0-9_]+", "_", meta.get("name", "").lower()).strip("_")
    if not name or not m.group(2).strip():
        return None
    tools = [TOOL_NAMES.get(t.strip().lower(), t.strip()) for t in meta.get("tools", "").split(",") if t.strip()]
    turns = meta.get("maxturns") or meta.get("max_turns") or ""
    return SubagentDefinition(
        name=name,
        description=meta.get("description") or f"Custom agent {name}",
        goal_prompt=m.group(2).strip(),
        allowed_tools=list(dict.fromkeys(tools)) or None,
        max_turns=int(turns) if turns.isdigit() else 20,
        role="fast" if meta.get("model", "").lower() == "haiku" else "default",
    )


def load_agent_files(cwd) -> dict[str, tuple[SubagentDefinition, Path]]:
    found: dict[str, tuple[SubagentDefinition, Path]] = {}
    for d in agent_dirs(cwd):  # later folders (the repo's own) override earlier ones
        for f in sorted(d.glob("*.md")) if d.is_dir() else []:
            try:
                definition = parse_agent_file(f.read_text(encoding="utf-8", errors="replace"))
            except OSError:
                continue
            if definition:
                found[definition.name] = (definition, f)
    return found


def get_default_subagent_definitions(config: Config | None = None) -> list[SubagentDefinition]:
    """The agents a repo's profile or config declares first, then the built-ins it did not already cover, then the user's
    agent files. A profile that brings its own investigator replaces the built-in one rather than doubling it."""
    declared = [SubagentDefinition(**spec.model_dump()) for spec in config.subagents] if config and config.subagents else []
    covers_search = any(re.search(r"investigat|explor", d.name) for d in declared)
    defs = {d.name: d for d in declared}
    for d in (CODEBASE_INVESTIGATOR, PLANNER, CODE_REVIEWER, GENERAL):
        if d.name not in defs and not (d is CODEBASE_INVESTIGATOR and covers_search):
            defs[d.name] = d
    if config:
        defs.update({n: d for n, (d, _) in load_agent_files(config.cwd).items()})
    return list(defs.values())
