"""Dependency-aware parallel execution.

Each task runs in its own git worktree on its own branch, so workers can never
corrupt each other's files or the user's working tree. Results are merged into
an integration branch in dependency order. Overlapping edits are detected,
real merge conflicts are never resolved by guessing: the conflicted task is
re-run on top of the integrated state with the original request in view, and
the report says so. The user's checkout is only touched with apply=True and
only when it is clean."""
from __future__ import annotations

import asyncio
import json
import shutil
import tempfile
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path

from agent.agent import Agent
from agent.events import AgentEventType
from config.config import Config


WORKER_TIMEOUT = 900  # seconds one worker may take before it is failed and the rest carry on


class GraphError(Exception):
    pass


@dataclass
class Node:
    id: str
    goal: str
    deps: list[str] = field(default_factory=list)
    files: list[str] = field(default_factory=list)  # ownership hint, not a lock
    status: str = "pending"  # pending|done|failed|conflict|resolved|skipped
    branch: str = ""
    changed: list[str] = field(default_factory=list)
    seconds: float = 0.0
    note: str = ""
    task_status: str | None = None  # the worker's own verification outcome


def parse_graph(data: dict) -> list[Node]:
    nodes = [Node(t["id"], t["goal"], list(t.get("deps", [])), list(t.get("files", [])))
             for t in data.get("tasks", [])]
    ids = [n.id for n in nodes]
    if not nodes:
        raise GraphError("graph has no tasks")
    if len(set(ids)) != len(ids):
        raise GraphError("duplicate task ids")
    for n in nodes:
        for d in n.deps:
            if d not in ids:
                raise GraphError(f"{n.id} depends on unknown task {d}")
    waves(nodes)  # raises on cycles
    return nodes


def waves(nodes: list[Node]) -> list[list[Node]]:
    """Topological layers: every node's deps sit in an earlier layer."""
    done: set[str] = set()
    remaining = list(nodes)
    out: list[list[Node]] = []
    while remaining:
        layer = [n for n in remaining if set(n.deps) <= done]
        if not layer:
            raise GraphError("dependency cycle among: " + ", ".join(n.id for n in remaining))
        out.append(layer)
        done |= {n.id for n in layer}
        remaining = [n for n in remaining if n.id not in done]
    return out


async def git(cwd: Path, *args: str) -> tuple[int, str]:
    p = await asyncio.create_subprocess_exec(
        "git", *args, cwd=cwd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT
    )
    out, _ = await p.communicate()
    return p.returncode, out.decode("utf-8", "replace").strip()


class Orchestrator:
    def __init__(self, config: Config, confirmation_callback=None, max_workers: int = 4,
                 resolve_conflicts: bool = True, tracer=None, progress=None):
        self.config = config
        self.cb = confirmation_callback
        self.sem = asyncio.Semaphore(max_workers)
        self.git_lock = asyncio.Lock()  # worktree add/remove touch shared .git metadata
        self.resolve = resolve_conflicts
        self.run_id = uuid.uuid4().hex[:6]
        self.tmp = Path(tempfile.mkdtemp(prefix=f"mew-{self.run_id}-"))
        self.repo = config.cwd
        self.integration = ""
        self.int_dir = self.tmp / "integration"
        self._worktrees: list[Path] = []
        self.tracer = tracer
        self.progress = progress  # (call_id, info) -> None, so the UI can show each worker live
        self.overlaps: dict[str, list[str]] = {}
        self.combined_checks: list[dict] = []

    def _emit(self, event: str, **kw) -> None:
        if self.tracer:
            self.tracer.emit(event, run=self.run_id, **kw)

    async def _worktree(self, name: str, base: str) -> Path:
        path = self.tmp / name
        async with self.git_lock:
            code, out = await git(self.repo, "worktree", "add", "-q", "-b", f"mew/{self.run_id}/{name}", str(path), base)
        if code:
            raise GraphError(f"git worktree failed: {out}")
        self._worktrees.append(path)
        return path

    async def _run_worker(self, node: Node, request: str, base: str) -> None:
        async with self.sem:
            started = time.perf_counter()
            node.branch = f"mew/{self.run_id}/{node.id}"
            wt = await self._worktree(node.id, base)
            git_dir = str((self.repo / ".git").resolve())  # a worktree's git metadata lives in the main repo's .git
            cfg = Config(**{**self.config.to_dict(), "cwd": str(wt),
                            "sandbox_write_extra": [*self.config.sandbox_write_extra, git_dir]})
            owned = f"\nFiles you own: {', '.join(node.files)}. Do not edit other files." if node.files else ""
            prompt = (
                f"You are one worker in a parallel job. Overall request (for context only):\n{request}\n\n"
                f"YOUR task: {node.goal}{owned}\n"
                "Other workers handle the rest in separate checkouts; do only your task, keep the change small."
            )
            last_text = ""
            say = (lambda **info: self.progress(f"{self.run_id}:{node.id}", {"agent": node.id, **info})) if self.progress else (lambda **_: None)
            say(kind="start", goal=node.goal)
            try:
                async def work() -> None:
                    nonlocal last_text
                    async with Agent(cfg, self.cb) as agent:
                        async for ev in agent.run(prompt):
                            if ev.type == AgentEventType.TOOL_CALL_START:
                                say(kind="tool", name=ev.data.get("name"), args=ev.data.get("arguments", {}), tokens=agent._spent())
                            if ev.type == AgentEventType.TEXT_COMPLETE:
                                last_text = ev.data.get("content") or ""
                            if ev.type == AgentEventType.AGENT_END:
                                node.task_status = (ev.data.get("report") or {}).get("status")

                # one stuck worker must not hold the whole layer (and the merge) hostage
                await asyncio.wait_for(work(), WORKER_TIMEOUT)
                node.status = "failed" if node.task_status in ("failed", "verification_failed") else "done"
                if node.status == "failed":
                    node.note = f"worker ended {node.task_status}"
            except asyncio.TimeoutError:
                node.status, node.note = "failed", f"timed out after {WORKER_TIMEOUT}s"
            except Exception as e:  # one worker crashing must not sink the others
                node.status, node.note = "failed", f"{type(e).__name__}: {e}"
            await git(wt, "add", "-A")
            await git(wt, "-c", "user.email=mew@local", "-c", "user.name=mew", "commit", "-qm", f"{node.id}: {node.goal[:60]}")
            _, names = await git(wt, "diff", "--name-only", f"{base}", "HEAD")
            node.changed = [n for n in names.splitlines() if n]
            if node.status == "done" and not node.changed:
                # a worker that "finished" without touching anything almost
                # always printed code instead of writing it; never count that as success
                node.status = "failed"
                node.note = f"worker made no changes; it said: {last_text[:200]!r}"
            node.seconds = round(time.perf_counter() - started, 2)
            say(kind="done", ok=node.status == "done")
            self._emit("worker_done", node=node.id, status=node.status, changed=node.changed, sec=node.seconds)

    async def _merge(self, node: Node) -> bool:
        code, out = await git(self.int_dir, "-c", "user.email=mew@local", "-c", "user.name=mew",
                              "merge", "--no-ff", "-m", f"merge {node.id}", node.branch)
        if code == 0:
            return True
        _, conflicted = await git(self.int_dir, "diff", "--name-only", "--diff-filter=U")
        node.note = "merge conflict in: " + ", ".join(conflicted.split()) if conflicted else out[-200:]
        await git(self.int_dir, "merge", "--abort")
        return False

    async def run(self, request: str, nodes: list[Node], apply: bool = False) -> dict:
        t0 = time.perf_counter()
        code, head = await git(self.repo, "rev-parse", "HEAD")
        if code:
            raise GraphError("parallel mode needs a git repository with at least one commit")
        self.integration = f"mew/{self.run_id}/integration"
        try:
            async with self.git_lock:
                await git(self.repo, "worktree", "add", "-q", "-b", self.integration, str(self.int_dir), head)
            self._worktrees.append(self.int_dir)
            by_id = {n.id: n for n in nodes}

            for layer in waves(nodes):
                runnable = [n for n in layer if all(by_id[d].status in ("done", "resolved") for d in n.deps)]
                for n in layer:
                    if n not in runnable:
                        n.status, n.note = "skipped", "a dependency did not complete"
                _, base = await git(self.int_dir, "rev-parse", "HEAD")
                await asyncio.gather(*(self._run_worker(n, request, base) for n in runnable))

                # overlap report: same file touched by several workers in this layer
                touched: dict[str, list[str]] = {}
                for n in runnable:
                    for f in n.changed:
                        touched.setdefault(f, []).append(n.id)
                self.overlaps.update({f: ids for f, ids in touched.items() if len(ids) > 1})

                conflicted: list[Node] = []
                for n in runnable:  # layer order == graph order, deterministic
                    if n.status != "done" or not n.changed:
                        continue
                    if not await self._merge(n):
                        n.status = "conflict"
                        conflicted.append(n)
                for n in conflicted:
                    if not self.resolve:
                        continue
                    await self._rerun_on_integrated(n, request)

                await self._combined_check(runnable)

            ok_states = {"done", "resolved"}
            all_ok = all(n.status in ok_states for n in nodes)
            applied = "not requested"
            if apply and all_ok:
                applied = await self._apply()
            status = "ok" if all_ok else ("conflict" if any(n.status == "conflict" for n in nodes) else "partial")
            wall = time.perf_counter() - t0
            return {
                "status": status,
                "integration_branch": self.integration,
                "applied": applied,
                "wall_sec": round(wall, 2),
                "sum_worker_sec": round(sum(n.seconds for n in nodes), 2),
                "overlapping_files": self.overlaps,
                "combined_checks": self.combined_checks,
                "tasks": [vars(n) for n in nodes],
            }
        finally:
            await self._cleanup()

    async def _rerun_on_integrated(self, node: Node, request: str) -> None:
        """A merge conflict means two workers made incompatible edits. Re-run
        the loser on top of what is already integrated, with the full request,
        so the reconciliation follows the original intent."""
        _, base = await git(self.int_dir, "rev-parse", "HEAD")
        saved = node.note
        node.goal += (
            "\n\nNOTE: an earlier attempt at this task conflicted with changes from another worker that are now "
            "in your checkout. Reconcile with them so the ORIGINAL REQUEST is satisfied; do not blindly overwrite."
        )
        node.id, orig_id = f"{node.id}-retry", node.id
        await self._run_worker(node, request, base)
        node.id = orig_id
        if node.status == "done" and await self._merge(node):
            node.status, node.note = "resolved", f"re-run after conflict ({saved}); review recommended"
        else:
            node.status = "conflict"

    async def _combined_check(self, ran: list[Node]) -> None:
        from agent.verify import run_check, select_checks

        files = sorted({f for n in ran for f in n.changed})
        cfg = Config(**{**self.config.to_dict(), "cwd": str(self.int_dir)})
        for cmd, sub in select_checks(cfg, files):
            r = await run_check(cmd, self.int_dir / sub, cfg)
            self.combined_checks.append(r.to_dict())

    async def _apply(self) -> str:
        _, dirty = await git(self.repo, "status", "--porcelain")
        if dirty:
            return f"skipped: working tree has uncommitted changes; merge branch {self.integration} yourself"
        code, out = await git(self.repo, "merge", "--ff-only", self.integration)
        return "fast-forwarded" if code == 0 else f"skipped: {out[-150:]}"

    async def _cleanup(self) -> None:
        async with self.git_lock:
            for wt in reversed(self._worktrees):
                await git(self.repo, "worktree", "remove", "--force", str(wt))
            _, branches = await git(self.repo, "branch", "--list", f"mew/{self.run_id}/*")
            for b in branches.split():
                if b.endswith("/integration") or b == "*":
                    continue
                await git(self.repo, "branch", "-D", b)
        shutil.rmtree(self.tmp, ignore_errors=True)


PLAN_PROMPT = """Split the request into tasks for parallel workers, each in a separate git checkout.
Reply with ONLY JSON: {"tasks":[{"id":"short-id","goal":"complete instruction","deps":["other-id"],"files":["path"]}]}
Rules: split ONLY if parts touch different files and can be done independently; otherwise return ONE task.
Use deps when one task needs another's output. Never invent files outside the request's scope. Max 6 tasks."""


async def plan(client, request: str) -> list[Node]:
    """Ask a model to decompose; any invalid answer degrades to one task."""
    from client.response import StreamEventType

    text = ""
    async for ev in client.chat_completion(
        [{"role": "system", "content": PLAN_PROMPT}, {"role": "user", "content": request}],
        stream=False, role="plan",
    ):
        if ev.type == StreamEventType.MESSAGE_COMPLETE and ev.text_delta:
            text += ev.text_delta.content
    try:
        raw = text[text.index("{"): text.rindex("}") + 1]
        return parse_graph(json.loads(raw))
    except (ValueError, KeyError, GraphError, TypeError):
        return [Node("main", request)]
