from pydantic import BaseModel, Field

from agent.orchestrator import GraphError, Orchestrator, parse_graph
from config.config import ApprovalPolicy
from tools.base import Tool, ToolConfirmation, ToolInvocation, ToolKind, ToolResult


class TaskItem(BaseModel):
    id: str = Field(..., description="Short unique id")
    goal: str = Field(..., description="Complete, self-contained instruction for one worker")
    files: list[str] = Field(default_factory=list, description="Files this worker owns (it must not edit others)")
    deps: list[str] = Field(default_factory=list, description="Ids of tasks that must finish first")


class ParallelParams(BaseModel):
    tasks: list[TaskItem] = Field(..., min_length=2, description="2 or more tasks")
    apply: bool = Field(True, description="Fast-forward the merged result into the working tree when it is clean")


class ParallelTasksTool(Tool):
    name = "parallel_tasks"
    description = (
        "Run 2+ coding tasks at the same time, each worker in its own git worktree, then merge them in dependency order "
        "and run the project checks. Use ONLY for work on different files that does not depend on each other; "
        "for exploration use sub-agents instead. Conflicts are detected and reported, never silently resolved."
    )
    kind = ToolKind.WRITE
    schema = ParallelParams

    async def get_confirmation(self, invocation: ToolInvocation) -> ToolConfirmation | None:
        p = ParallelParams(**invocation.params)
        rows = "\n".join(f"  {t.id}: {t.goal[:110]}" + (f"  [{', '.join(t.files)}]" if t.files else "") for t in p.tasks)
        return ToolConfirmation(
            tool_name=self.name, params=invocation.params,
            description=f"Run {len(p.tasks)} tasks in parallel (separate git worktrees):\n{rows}",
            affected_paths=[invocation.cwd],
        )

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = ParallelParams(**invocation.params)
        try:
            nodes = parse_graph({"tasks": [t.model_dump() for t in p.tasks]})
        except GraphError as e:
            return ToolResult.error_result(f"Invalid task graph: {e}")
        # the user approved this plan, so workers may edit inside their own worktree; no recursion
        cfg = self.config.model_copy(update={
            "approval": ApprovalPolicy.AUTO_EDIT, "enable_parallel_tasks": False, "subagents": [], "auto_review": False,
        })
        request = "; ".join(t.goal for t in p.tasks)
        try:
            res = await Orchestrator(cfg, max_workers=self.config.max_parallel_agents, progress=invocation.progress).run(request, nodes, apply=p.apply)
        except GraphError as e:
            return ToolResult.error_result(str(e))
        lines = [f"Parallel run: {res['status']} in {res['wall_sec']}s (sum of worker time {res['sum_worker_sec']}s)"]
        for t in res["tasks"]:
            files = ", ".join(t["changed"][:6]) or "no changes"
            lines.append(f"- {t['id']}: {t['status']} ({t['seconds']}s) {files}" + (f"; {t['note']}" if t["note"] else ""))
        if res["overlapping_files"]:
            lines.append(f"Files edited by several workers: {res['overlapping_files']}")
        for c in res["combined_checks"]:
            lines.append(f"check {c['command']}: {'ok' if c['ok'] else 'FAILED'}")
        lines.append(f"Integration branch: {res['integration_branch']}; applied to the working tree: {res['applied']}")
        ok = res["status"] == "ok"
        return (ToolResult.success_result if ok else ToolResult.error_result)("\n".join(lines))
