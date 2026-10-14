import asyncio
import re
import json
import os
import signal
import time
from pathlib import Path
import sys
import click
from prompt_toolkit.filters import Condition

from agent.agent import Agent
from agent.events import AgentEventType
from agent.persistence import PersistenceManager, SessionSnapshot
from agent.session import Session
from config.config import ApprovalPolicy, Config
from config.loader import load_config
from config.loader import get_data_dir, load_last_model, save_last_model
from ui.prompt import COMMANDS, InputSession, expand_mentions
from ui.tui import TUI, context_bar, get_console
from utils.cost import summarize_usage
from utils.templates import expand, load_templates, split_args

console = get_console()


class CLI:
    def __init__(self, config: Config):
        self.agent: Agent | None = None
        self.resume_id: str | None = None  # --resume / --continue: picked up when the interactive session starts
        self.config = config
        self.tui = TUI(config, console)
        self.last_report: dict | None = None

    async def run_single(self, message: str, report_path: Path | None = None) -> str | None:
        # On a real terminal, ask when something needs approval instead of refusing it.
        # Piped or CI runs have nobody to answer, so they keep failing closed.
        ask = self.tui.confirm if sys.stdin.isatty() and sys.stdout.isatty() else None
        async with Agent(self.config, confirmation_callback=ask) as agent:
            self.agent = agent
            result = await self._process_message(message)
            if report_path:
                self._write_report(report_path, agent, result)
            return result

    async def run_json(self, message: str) -> bool:
        """One event per stdout line, for CI and scripts: {"type": "tool_call_start", ...}.
        No TUI, so pair it with --approval auto (nothing can ask a human)."""
        ok = True
        async with Agent(self.config) as agent:
            async for event in agent.run(message):
                print(json.dumps({"type": event.type.value, **event.data}, default=str), flush=True)
                if event.type == AgentEventType.AGENT_ERROR:
                    ok = False
                elif event.type == AgentEventType.AGENT_END:
                    self.last_report = event.data.get("report")
        return ok and (self.last_report or {}).get("status") not in {"failed", "verification_failed"}

    async def run_rpc(self, lines=None) -> None:
        """Drive mew from another program: one JSON command per stdin line, events and replies on stdout.

            {"type": "prompt", "message": "fix the bug", "id": "1"}   start a request (one at a time)
            {"type": "steer", "message": "use python 3.12"}           note read after the current tool calls
            {"type": "abort"}                                          stop the running request
            {"type": "get_state"}                                      is it running, which model

        Every command gets {"type": "response", "command": ..., "success": ..., "id": ...}.
        Nobody can answer an approval prompt here, so pair it with --approval auto."""
        if lines is None:
            async def lines():
                loop = asyncio.get_running_loop()
                while line := await loop.run_in_executor(None, sys.stdin.readline):
                    yield line

        def emit(obj: dict) -> None:
            print(json.dumps(obj, default=str), flush=True)

        async with Agent(self.config) as agent:
            running: asyncio.Task | None = None

            async def work(message: str) -> None:
                try:
                    async for event in agent.run(message):
                        emit({"type": event.type.value, **event.data})
                except asyncio.CancelledError:
                    agent.session.context_manager.repair_orphans()
                    emit({"type": "aborted"})

            async for line in lines():
                if not line.strip():
                    continue
                try:
                    cmd = json.loads(line)
                    kind, rid = cmd["type"], cmd.get("id")
                except (ValueError, KeyError, TypeError):
                    emit({"type": "response", "success": False, "error": "each line must be a JSON object with a type"})
                    continue
                busy = running is not None and not running.done()
                error = None
                if kind == "prompt":
                    if busy:
                        error = "a request is already running; steer or abort it"
                    elif not cmd.get("message"):
                        error = "prompt needs a message"
                    else:
                        running = asyncio.create_task(work(cmd["message"]))
                elif kind == "steer":
                    if not busy:
                        error = "nothing is running to steer; send a prompt"
                    else:
                        agent.steering.append(str(cmd.get("message", "")))
                elif kind == "abort":
                    if busy:
                        running.cancel()
                elif kind == "get_state":
                    emit({"type": "state", "running": busy, "model": self.config.model_name, "session": agent.session.session_id})
                else:
                    error = f"unknown command: {kind}"
                emit({"type": "response", "command": kind, "id": rid, "success": error is None, **({"error": error} if error else {})})
            if running:
                await running  # stdin closed: let the last request finish

    async def run_orchestrated(self, message: str, graph: Path | None, apply: bool, report_path: Path | None) -> bool:
        from agent.orchestrator import Orchestrator, parse_graph, plan
        from agent.session import Session
        from client.llm_client import LLMClient

        client = LLMClient(self.config)
        await client.initialize()
        try:
            nodes = parse_graph(json.loads(graph.read_text())) if graph else await plan(client, message)
        finally:
            await client.close()
        console.print(f"[bold]Parallel plan:[/bold] {', '.join(n.id for n in nodes)}")
        result = await Orchestrator(self.config).run(message, nodes, apply=apply)
        console.print_json(json.dumps({k: v for k, v in result.items() if k != "tasks"}))
        for t in result["tasks"]:
            console.print(f"  {t['status']:<9}{t['id']:<14}{t['seconds']}s  {t['note']}")
        if report_path:
            report_path.write_text(json.dumps({"report": {"status": result["status"], "repairs": 0,
                "changed_files": sorted({f for t in result["tasks"] for f in t["changed"]}), "checks": result["combined_checks"]},
                "parallel": result, "model_calls": None}, indent=1))
        return result["status"] == "ok"

    def _write_report(self, path: Path, agent: Agent, response: str | None) -> None:
        """Metrics for one run, for the benchmark runner and for debugging."""
        import json

        ev = agent.session.tracer.events
        calls = [e for e in ev if e["event"] == "model_call"]
        usage = agent.session.context_manager.total_usage
        path.write_text(
            json.dumps(
                {
                    "report": self.last_report,
                    "final_response": response,
                    "session_id": agent.session.session_id,
                    "turns": agent.session.turn_count,
                    "model_calls": len(calls),
                    "model_call_failures": sum(1 for c in calls if not c["ok"]),
                    "models_used": sorted({c["model"] for c in calls if c["ok"]}),
                    "tool_calls": sum(1 for e in ev if e["event"] == "tool_call"),
                    "tool_failures": sum(
                        1 for e in ev if e["event"] == "tool_call" and not e["ok"]
                    ),
                    "ttft_sec": [c["ttft"] for c in calls if c.get("ttft") is not None],
                    "usage": usage.__dict__,
                },
                indent=1,
            )
        )

    def _toolbar(self) -> str:
        """Footer: the things you act on first (mode, model, context), details after, fitted to the width."""
        import shutil

        sess = self.agent.session if self.agent else None
        parts = []
        from tools.builtin.shell import JOBS

        running = sum(not j.done for j in JOBS.values())
        if running:
            parts.append(f"⚙ {running} running (/jobs)")
        if sess:
            parts.append(sess.approval_manager.approval_policy.value)
        model = (sess.client.last_model if sess and sess.client.last_model else self.config.model_name)
        parts.append(model)
        if sess:
            used = sess.context_manager._latest_usage.total_tokens
            window = sess.client.registry.get(self.config.model_name).context_length
            if used:
                parts.append("ctx " + context_bar(100 * used / window))
            u = summarize_usage(sess.tracer.events, self.config.pricing)
            if u["calls"]:
                parts.append(f"{u['prompt_tokens'] / 1000:.0f}k in ({u['cache_hit_pct']:.0f}% cached)")
                if u["est_cost_usd"] is not None:
                    parts.append(f"${u['est_cost_usd']}")
            if self.config.sandbox != "off":
                from safety.sandbox import mode

                parts.append({"on": "sandboxed", "unavailable": "no sandbox"}.get(mode(self.config), ""))
        if self.config.profile:
            parts.append(self.config.profile)
        text = "  ·  ".join(p for p in parts if p)
        width = shutil.get_terminal_size((100, 20)).columns - 3
        return text if len(text) <= width else text[: width - 1] + "…"

    async def run_interactive(self) -> str | None:
        screen = None
        if sys.stdout.isatty() and sys.stdin.isatty() and not os.environ.get("MEW_PLAIN"):
            from ui.screen import Screen

            screen = Screen(self.tui, console, lambda: f"{self.config.model_name}  ·  {str(self.config.cwd).replace(str(Path.home()), '~')}")  # MEW_PLAIN=1 keeps the old scrolling layout
        self.tui.print_welcome(
            "mew",
            lines=[
                f"model: {self.config.model_name}   approval: {self.config.approval.value}",
                f"cwd: {str(self.config.cwd).replace(str(Path.home()), '~')}" + (f"   profile: {self.config.profile}" if self.config.profile else ""),
                "/help for commands · @file to attach · !cmd to run a command · Alt+Enter for a newline",
            ],
        )

        async with Agent(self.config, confirmation_callback=self.tui.confirm) as agent:
            self.agent = agent
            reg = agent.session.client.registry
            inputs = InputSession(
                self.config.cwd, self.config.forbidden_paths, lambda: [m.id for m in reg.all()],
                self._toolbar, get_data_dir() / "input_history",
                **({"body": screen.body, "extra_kb": screen.kb, "mouse": Condition(lambda: screen.mouse_on)} if screen else {}),
            )
            self.inputs = inputs
            inputs.is_busy = lambda: self.tui._turn_active
            inputs.on_steer = agent.steering.append
            if self.resume_id:
                if await self._resume(self.resume_id):
                    console.print(f"[success]Resumed {self.resume_id} ({self.agent.session.turn_count} turns). Carry on where you left off.[/success]")
                else:
                    console.print(f"[error]No session with id {self.resume_id}; starting a new one.[/error]")
            if screen:
                import ui.prompt as prompt_mod

                prompt_mod.ACTIVE = inputs

            while True:
                try:
                    user_input = (await inputs.read()).strip()
                    if not user_input:
                        continue
                    if user_input.startswith(("/", "!")):
                        self.tui.echo_user(user_input)  # show what was typed so Enter always gets an answer
                    if user_input.startswith("/"):
                        if not await self._handle_command(user_input):
                            break
                        continue
                    if user_input.startswith("!"):
                        await self._run_bang(user_input)
                        continue
                    self.tui.echo_user(user_input)
                    text, attached = expand_mentions(user_input, self.config.cwd, self.config.forbidden_paths)
                    if attached:
                        console.print(f"[dim]  attached: {', '.join(attached)}[/dim]")
                    await self._run_turn(text)
                except KeyboardInterrupt:
                    console.print("[dim]  (Ctrl+C) type /exit to quit[/dim]")
                except EOFError:
                    break
            self._autosave()  # the agent (and its session) is released when this block ends
            sess = self.agent.session
            self._exit_info = (sess.session_id, sess.turn_count)

        if screen:
            import ui.prompt as prompt_mod

            prompt_mod.ACTIVE = None
            await inputs.close()
            screen.stop()
            console.file = sys.__stdout__
        console.print("\n[dim]Goodbye![/dim]")
        sid, turns = getattr(self, "_exit_info", (None, 0))
        if turns:
            console.print(f"[dim]Resume this session:  [/dim][bold]mew --resume {sid}[/bold]")
            console.print("[dim]Or the newest one in this folder:  [/dim][bold]mew --continue[/bold]")

    async def _run_turn(self, text: str) -> None:
        """Run one request as a task so Ctrl+C cancels the agent, not the app."""
        task = asyncio.create_task(self._process_message(text))
        inputs = getattr(self, "inputs", None)
        if inputs:
            inputs.on_interrupt = task.cancel  # full-screen mode gets Ctrl+C as a key, not a signal
        loop = asyncio.get_running_loop()
        try:
            loop.add_signal_handler(signal.SIGINT, task.cancel)
        except (NotImplementedError, RuntimeError):
            pass
        try:
            await task
        except asyncio.CancelledError:
            self.tui.end_turn()
            console.print("\n[warning]Interrupted[/warning]")
            self.agent.session.context_manager.repair_orphans()  # providers reject unanswered tool calls
        finally:
            self._autosave()
            if inputs:
                inputs.on_interrupt = None
                while self.agent.steering:  # the turn ended before the note could land: run it as the next request
                    inputs.queue.put_nowait(self.agent.steering.pop(0))
            self.tui.end_turn()
            try:
                loop.remove_signal_handler(signal.SIGINT)
            except (NotImplementedError, RuntimeError, ValueError):
                pass

    async def _run_bang(self, line: str) -> None:
        """`!cmd` runs a command for the user and shows the output to the model; `!!cmd` keeps it private."""
        from safety.guard import violation
        from tools.builtin.shell import build_environment

        private = line.startswith("!!")
        cmd = line.lstrip("!").strip()
        if not cmd:
            return
        proc = await asyncio.create_subprocess_shell(
            cmd, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT,
            cwd=self.config.cwd, env=build_environment(self.config),
        )
        try:
            out, _ = await asyncio.wait_for(proc.communicate(), 120)
        except asyncio.TimeoutError:
            proc.kill()
            out = b"[timed out after 120s]"
        text = out.decode("utf-8", "replace")
        console.print(text.rstrip(), markup=False, highlight=False)
        if not private and violation("shell", {"command": cmd}, self.config):
            console.print("[dim]  (protected by repository rules: output not shared with the model)[/dim]")
            private = True
        if not private:
            self.agent.session.context_manager.add_user_message(
                f"I ran `{cmd}` (exit {proc.returncode}):\n{text[:8000]}"
            )
            console.print("[dim]  (output added to the conversation)[/dim]")


    def _print_heaviest(self, u: dict) -> None:
        h = u.get("heaviest") or {}
        if u.get("calls", 0) < 6 or not h.get("top") or h.get("tool_result_carried", 0) < 5000:  # tiny tasks have nothing worth tuning
            return
        share = min(100, round(100 * h["tool_result_carried"] / u["prompt_tokens"]))
        console.print(f"[dim]    {share}% of the input tokens were earlier tool results re-sent. Biggest:[/dim]")
        for r in h["top"][:3]:
            console.print(f"[dim]      {r['tool']} {r['target']}: {r['tokens']:,} tokens, re-sent {r['carried'] // max(r['tokens'], 1)}x = {r['carried']:,}[/dim]")
        if h["repeated"]:
            console.print(f"[dim]    repeated: {'; '.join(h['repeated'])}[/dim]")

    def _print_report(self, report: dict | None, elapsed: float = 0.0) -> None:
        if not report:
            return
        u = report.get("usage") or {}
        self.tui.turn_summary(elapsed, u, report)
        quiet = self.tui.quiet
        w = u.get("cache_waste") or {}
        if w.get("misses") and not quiet:
            console.print(f"[dim]    cache misses: {w['missed_tokens']:,} tokens re-billed ({', '.join(w['reasons'])})[/dim]")
        if not quiet:
            self._print_heaviest(u)
        g = report.get("grounding") or {}
        if g.get("unverified"):
            console.print(f"[warning]  ⚠ {len(g['unverified'])} cited reference(s) could not be verified: {'; '.join(g['unverified'][:4])}[/warning]")
        if report["status"] == "done_no_changes":
            return
        style = {"verified": "success", "unverified": "warning"}.get(report["status"], "error")
        if quiet:
            bad = [c for c in report["checks"] if not c["ok"]]
            rv = report.get("review")
            tail = (f"  ·  {len(report['checks'])} checks ok" if report["checks"] and not bad else "") + (f"  ·  review {rv['verdict'].lower()}" if rv else "")
            console.print(f"  [{style}]{report['status']}[/{style}]  [dim]{len(report['changed_files'])} files{tail}  ·  /undo reverts[/dim]")
            for c in bad:
                console.print(f"    [error]✗[/] {c['command']}")
            return
        if report.get("undo"):
            console.print("[dim]    /undo puts these files back[/dim]")
        console.print(f"  [{style}]Result: {report['status']}[/{style}]  files changed: {len(report['changed_files'])}  repairs: {report['repairs']}")
        for c in report["checks"]:
            console.print(f"    [{'success' if c['ok'] else 'error'}]{'✓' if c['ok'] else '✗'}[/] {c['command']} ({c['seconds']}s)")
        if report["status"] == "verified" and report["checks"] and not any(
            re.search(r"test|spec|pytest|vitest|jest|playwright", c["command"]) for c in report["checks"]
        ):
            console.print("[dim]    These checks (types/lint) don't exercise the changed behaviour, so \"verified\" does not mean the bug is fixed.[/dim]")
        rv = report.get("review")
        if rv:
            console.print(f"    review: {rv['verdict']}" + ("" if rv["verdict"] == "PASS" else f" (see findings above)"))
        if report["status"] == "unverified":
            console.print("    [dim]No verification commands found; changes were not tested.[/dim]")

    def _get_tool_kind(self, tool_name: str) -> str | None:
        tool_kind = None
        tool = self.agent.session.tool_registry.get(tool_name)
        return tool.kind.value if tool else None

    def _snapshot(self) -> SessionSnapshot:
        sess = self.agent.session
        msgs = sess.context_manager.get_messages()
        first = next((m["content"] for m in msgs if m["role"] == "user" and m.get("content")), "")
        title = first.split("\n\n---\n")[0].strip().replace("\n", " ")[:70]  # drop the playbooks appended to a request
        return SessionSnapshot(
            session_id=sess.session_id, created_at=sess.created_at, updated_at=sess.updated_at, turn_count=sess.turn_count,
            messages=msgs, total_usage=sess.context_manager.total_usage, cwd=str(self.config.cwd), title=title,
        )

    def _autosave(self) -> None:
        """Written after every turn, so a crash or closed terminal still leaves something to resume."""
        try:
            if self.agent and self.agent.session.turn_count:
                PersistenceManager().save_session(self._snapshot())
        except Exception:
            pass

    async def _resume(self, session_id: str) -> bool:
        snapshot = PersistenceManager().load_session(session_id)
        if not snapshot:
            return False
        session = Session(config=self.config)
        await session.initialize()
        session.session_id, session.created_at = snapshot.session_id, snapshot.created_at
        session.updated_at, session.turn_count = snapshot.updated_at, snapshot.turn_count
        session.context_manager.total_usage = snapshot.total_usage
        cm = session.context_manager
        for msg in snapshot.messages:
            if msg.get("role") == "user":
                cm.add_user_message(msg.get("content", ""))
            elif msg.get("role") == "assistant":
                cm.add_assistant_message(msg.get("content", ""), msg.get("tool_calls"))
            elif msg.get("role") == "tool":
                cm.add_tool_result(msg.get("tool_call_id", ""), msg.get("content", ""))
        cm.repair_orphans()  # a session saved mid-turn may end on an unanswered tool call
        # the new session starts with no one to ask; without this a resumed run refuses every edit
        session.approval_manager.confirmation_callback = self.agent.session.approval_manager.confirmation_callback
        await self.agent.session.client.close()
        await self.agent.session.mcp_manager.shutdown()
        self.agent.session = session
        return True

    async def _process_message(self, message: str) -> str | None:
        if not self.agent:
            return None

        final_response: str | None = None
        started = time.monotonic()
        reg = self.agent.session.tool_registry
        reg.progress = self.tui.agent_progress  # live sub-agent status
        self.tui.start_turn()
        try:
            async for event in self.agent.run(message):
                t = event.type
                if t == AgentEventType.TEXT_DELTA:
                    self.tui.assistant_delta(event.data.get("content", ""))
                elif t == AgentEventType.TEXT_COMPLETE:
                    final_response = event.data.get("content")
                    self.tui.assistant_flush()
                elif t == AgentEventType.NOTICE:
                    self.tui.notice(event.data.get("message", ""))
                elif t == AgentEventType.AGENT_ERROR:
                    self.tui.error(event.data.get("error", "Unknown error"))
                elif t == AgentEventType.TOOL_CALL_START:
                    name = event.data.get("name", "unknown")
                    self.tui.tool_call_start(event.data.get("call_id", ""), name, self._get_tool_kind(name), event.data.get("arguments", {}))
                elif t == AgentEventType.TOOL_CALL_COMPLETE:
                    name = event.data.get("name", "unknown")
                    self.tui.tool_call_complete(
                        event.data.get("call_id", ""), name, self._get_tool_kind(name), event.data.get("success", False),
                        event.data.get("output", ""), event.data.get("error"), event.data.get("metadata"),
                        event.data.get("diff"), event.data.get("truncated", False), event.data.get("exit_code"),
                    )
                    todos = reg.get("todos")
                    if todos is not None and name == "todos":
                        self.tui.set_todos(todos.snapshot())
                elif t == AgentEventType.AGENT_END:
                    self.last_report = event.data.get("report")
        finally:
            self.tui.end_turn()
        self._print_report(self.last_report, time.monotonic() - started)
        return final_response

    async def _handle_command(self, command: str) -> bool:
        cmd = command.lower().strip()
        parts = cmd.split(maxsplit=1)
        cmd_name = parts[0]
        # args keep their original case: model ids like Qwen/Qwen3.8-Max are case-sensitive
        raw_parts = command.strip().split(maxsplit=1)
        cmd_args = raw_parts[1] if len(raw_parts) > 1 else ""
        if cmd_name == "/exit" or cmd_name == "/quit":
            return False
        elif command == "/help":
            self.tui.show_help()
        elif cmd_name == "/compact":
            self.tui.start_turn("Compacting")
            ok = await self.agent._compact()
            self.tui.end_turn()
            console.print("[success]Context compacted[/success]" if ok else "[dim]Nothing to compact yet[/dim]")
        elif cmd_name == "/cost":
            u = summarize_usage(self.agent.session.tracer.events, self.config.pricing)
            w = u["cache_waste"]
            console.print(f"  calls: {u['calls']}   in: {u['prompt_tokens']:,} ({u['cache_hit_pct']}% cached)   out: {u['completion_tokens']:,}")
            console.print(f"  est cost: {('$' + str(u['est_cost_usd'])) if u['est_cost_usd'] is not None else 'unknown (add a \\[pricing] section to config)'}")
            self._print_heaviest(u)
            if w["misses"]:
                console.print(f"  cache misses: {w['misses']} ({w['missed_tokens']:,} tokens re-billed: {', '.join(w['reasons'])})")
        elif cmd_name == "/verbose":
            self.tui.quiet = not self.tui.quiet
            console.print(f"[dim]  output: {'quiet' if self.tui.quiet else 'verbose'}[/dim]")
        elif cmd_name == "/more":
            if not self.agent.capped_last:
                console.print("[dim]  /more only applies after a request stopped at its call budget.[/dim]")
                return True
            self.agent.extra_calls = 10
            await self._run_turn("Continue from where you stopped. You may use 10 more tool calls; spend them on what is still unknown, then answer.")
        elif cmd_name == "/challenge":
            if not cmd_args:
                console.print("[dim]Usage: /challenge <your idea or plan>. A second model argues against it.[/dim]")
            else:
                from agent.challenge import challenge_request, render

                self.tui.start_turn("Challenging")
                objections, dropped, err = await challenge_request(self.config, cmd_args)
                self.tui.end_turn()
                console.print(f"[error]Challenger failed: {err[:200]}[/error]" if err else (render(objections, dropped) or "[dim]Nothing it could back with evidence.[/dim]"))
        elif cmd_name == "/skills":
            for sk in self.agent.session.skills:
                tag = " [dim](always on)[/dim]" if sk.always else ""
                console.print(f"  [accent]{sk.name}[/accent]{tag}  [dim]{sk.description}[/dim]")
            if not self.agent.session.skills:
                console.print("[dim]  no skills loaded[/dim]")
        elif cmd_name == "/copy":
            from ui.screen import copy_to_clipboard

            if self.tui.last_reply:
                copy_to_clipboard(self.tui.last_reply)
                console.print(f"[success]  copied {len(self.tui.last_reply)} characters[/success]")
            else:
                console.print("[dim]  nothing to copy yet[/dim]")
        elif cmd_name == "/jobs":
            from tools.builtin.shell import JOBS, kill_job

            words = cmd_args.split()
            kill = words[:1] == ["kill"]
            job = JOBS.get(int(words[-1])) if words and words[-1].isdigit() else None
            if kill and job:
                kill_job(job)
                console.print(f"[dim]  stopped job {job.id}[/dim]")
            elif job:
                console.print(f"[accent]job {job.id}[/accent] [dim]{job.command}[/dim]\n{job.tail(40)}")
            elif not JOBS:
                console.print("[dim]  no background jobs[/dim]")
            for j in JOBS.values() if not job else []:
                code = j.process.returncode
                state = "running" if code is None else "done" if code == 0 else f"failed ({code})"
                console.print(f"  [accent]{j.id}[/accent] {state:<12} [dim]{time.monotonic() - j.started:.0f}s  {j.command}[/dim]")
        elif cmd_name == "/expand":
            self.tui.show_result(int(cmd_args) if cmd_args.strip().isdigit() else 1)
        elif command == "/clear":
            self.agent.session.context_manager.clear()
            self.agent.session.loop_detector.clear()
            console.print("[success]Conversation cleared [/success]")
        elif command == "/config":
            console.print("\n[bold]Current Configuration[/bold]")
            console.print(f"  Model: {self.config.model_name}")
            console.print(f"  Temperature: {self.config.temperature}")
            console.print(f"  Approval: {self.config.approval.value}")
            console.print(f"  Working Dir: {self.config.cwd}")
            console.print(f"  Max Turns: {self.config.max_turns}")
            console.print(f"  Hooks Enabled: {self.config.hooks_enabled}")
        elif cmd_name == "/model":
            if cmd_args:
                known, near = self.agent.session.client.registry.validate(cmd_args)
                if not known:
                    console.print(f"[error]Unknown model: {cmd_args}[/error]")
                    if near:
                        console.print("Did you mean: " + ", ".join(near))
                else:
                    self.config.model_name = cmd_args
                    self.config.model_pinned = True
                    save_last_model(cmd_args)
                    console.print(f"[success]Model changed to: {cmd_args} [/success]")
            else:
                console.print(f"Current model: {self.config.model_name}")
        elif cmd_name == "/approval":
            if cmd_args:
                try:
                    approval = ApprovalPolicy(cmd_args)
                    self.config.approval = approval
                    self.agent.session.approval_manager.approval_policy = approval  # the manager holds its own copy
                    console.print(
                        f"[success]Approval policy changed to: {cmd_args} [/success]"
                    )
                except:
                    console.print(
                        f"[error]Incorrect approval policy: {cmd_args} [/error]"
                    )
                    console.print(
                        f"Valid options: {', '.join(p for p in ApprovalPolicy)}"
                    )
            else:
                console.print(f"Current approval policy: {self.config.approval.value}")
        elif cmd_name == "/stats":
            stats = self.agent.session.get_stats()
            console.print("\n[bold]Session Statistics [/bold]")
            for key, value in stats.items():
                console.print(f"   {key}: {value}")
        elif cmd_name == "/tools":
            tools = self.agent.session.tool_registry.get_tools()
            console.print(f"\n[bold]Available tools ({len(tools)}) [/bold]")
            for tool in tools:
                console.print(f"  • {tool.name}")
        elif cmd_name == "/mcp":
            mcp_servers = self.agent.session.mcp_manager.get_all_servers()
            console.print(f"\n[bold]MCP Servers ({len(mcp_servers)}) [/bold]")
            for server in mcp_servers:
                status = server["status"]
                status_color = "green" if status == "connected" else "red"
                console.print(
                    f"  • {server['name']}: [{status_color}]{status}[/{status_color}] ({server['tools']} tools)"
                )
        elif cmd_name == "/save":
            persistence_manager = PersistenceManager()
            session_snapshot = SessionSnapshot(
                session_id=self.agent.session.session_id,
                created_at=self.agent.session.created_at,
                updated_at=self.agent.session.updated_at,
                turn_count=self.agent.session.turn_count,
                messages=self.agent.session.context_manager.get_messages(),
                total_usage=self.agent.session.context_manager.total_usage,
            )
            persistence_manager.save_session(session_snapshot)
            console.print(
                f"[success]Session saved: {self.agent.session.session_id}[/success]"
            )
        elif cmd_name == "/sessions":
            persistence_manager = PersistenceManager()
            sessions = persistence_manager.list_sessions()
            console.print("\n[bold]Saved Sessions[/bold]")
            for s in sessions:
                here = " [dim](this folder)[/dim]" if s["cwd"] == str(self.config.cwd) else ""
                console.print(f"  • {s['session_id']}  {s['turn_count']} turns  {s['updated_at'][:16]}{here}\n      [dim]{s['title']}[/dim]")
        elif cmd_name == "/resume":
            sid = cmd_args.strip() or PersistenceManager().latest_for(str(self.config.cwd))
            if not sid:
                console.print("[dim]No saved session for this folder. /sessions lists all.[/dim]")
            elif await self._resume(sid):
                console.print(f"[success]Resumed {sid} ({self.agent.session.turn_count} turns)[/success]")
            else:
                console.print(f"[error]No session with id {sid}[/error]")
        elif cmd_name in ("/learn", "/lessons", "/forget"):
            from agent.learn import Learned

            learned = Learned(self.config.cwd)
            if cmd_name == "/learn":
                ok = bool(cmd_args) and learned.add_lesson(cmd_args)
                console.print("[success]Learned. It applies from the next session (the prompt is fixed for this one).[/success]" if ok else "[dim]Usage: /learn <a rule, at least a few words>[/dim]")
            elif cmd_name == "/forget":
                gone = learned.forget(int(cmd_args)) if cmd_args.strip().isdigit() else None
                console.print(f"[success]Forgot: {gone}[/success]" if gone else "[dim]Usage: /forget <number from /lessons>[/dim]")
            else:
                rows = learned.lessons()
                for i, text in enumerate(rows, 1):
                    console.print(f"  {i}. {text}")
                if not rows:
                    console.print("[dim]Nothing learned yet. Rejecting an action with feedback, or /learn <rule>, teaches mew.[/dim]")
                console.print(f"[dim]  {len(learned.data['edits'])} past verified run(s) also guide search.[/dim]")
        elif cmd_name == "/undo":
            info = self.agent.undo_info
            if not info:
                console.print("[dim]Nothing to undo: the last request changed no files.[/dim]")
            else:
                from ui.prompt import ask_choice
                from agent.verify import undo

                names = ", ".join(info["files"][:6]) + (" ..." if len(info["files"]) > 6 else "")
                pick = await ask_choice(console, f"Put back {len(info['files'])} file(s) to how they were before the last request? ({names})", ["Yes, undo", "No"])
                if pick == 0:
                    restored, deleted, skipped = await undo(self.config.cwd, info["ref"], info["files"], info["created"])
                    self.agent.undo_info = None
                    self.agent.session.tool_registry.epoch += 1  # files on disk changed under earlier reads
                    console.print(f"[success]Restored {len(restored)}, removed {len(deleted)} new file(s).[/success]")
                    if skipped:
                        console.print(f"[warning]Left alone (no earlier copy): {', '.join(skipped)}[/warning]")
        elif cmd_name == "/checkpoint":
            persistence_manager = PersistenceManager()
            session_snapshot = SessionSnapshot(
                session_id=self.agent.session.session_id,
                created_at=self.agent.session.created_at,
                updated_at=self.agent.session.updated_at,
                turn_count=self.agent.session.turn_count,
                messages=self.agent.session.context_manager.get_messages(),
                total_usage=self.agent.session.context_manager.total_usage,
            )
            checkpoint_id = persistence_manager.save_checkpoint(session_snapshot)
            console.print(f"[success]Checkpoint created: {checkpoint_id}[/success]")
        elif cmd_name == "/restore":
            if not cmd_args:
                console.print(f"[error]Usage: /restore <checkpoint_id> [/error]")
            else:
                persistence_manager = PersistenceManager()
                snapshot = persistence_manager.load_checkpoint(cmd_args)
                if not snapshot:
                    console.print(f"[error]Checkpoint does not exist [/error]")
                else:
                    session = Session(
                        config=self.config,
                    )
                    await session.initialize()
                    session.session_id = snapshot.session_id
                    session.created_at = snapshot.created_at
                    session.updated_at = snapshot.updated_at
                    session.turn_count = snapshot.turn_count
                    session.context_manager.total_usage = snapshot.total_usage

                    for msg in snapshot.messages:
                        if msg.get("role") == "system":
                            continue
                        elif msg["role"] == "user":
                            session.context_manager.add_user_message(
                                msg.get("content", "")
                            )
                        elif msg["role"] == "assistant":
                            session.context_manager.add_assistant_message(
                                msg.get("content", ""), msg.get("tool_calls")
                            )
                        elif msg["role"] == "tool":
                            session.context_manager.add_tool_result(
                                msg.get("tool_call_id", ""), msg.get("content", "")
                            )

                    await self.agent.session.client.close()
                    await self.agent.session.mcp_manager.shutdown()

                    self.agent.session = session
                    console.print(
                        f"[success]Restored checkpoint {cmd_args} into session {session.session_id}[/success]"
                    )
        else:
            templates = load_templates()
            if cmd_name[1:] in templates:
                await self._run_turn(expand(templates[cmd_name[1:]], split_args(cmd_args)))
            else:
                console.print(f"[error]Unknown command: {cmd_name}[/error]")

        return True


@click.command()
@click.argument("prompt", required=False)
@click.option(
    "--cwd",
    "-c",
    type=click.Path(exists=True, file_okay=False, path_type=Path),
    help="Current working directory",
)
@click.option("--parallel", is_flag=True, help="Let a planner split the request into parallel workers")
@click.option("--graph", type=click.Path(exists=True, path_type=Path), help="Task graph JSON (implies --parallel)")
@click.option("--apply/--no-apply", default=True, help="Fast-forward the integrated result into a clean checkout")
@click.option("--json", "json_mode", is_flag=True, help="Print agent events as JSON lines instead of the UI (needs a prompt)")
@click.option("--rpc", "rpc_mode", is_flag=True, help="Read JSON commands on stdin, write events on stdout (for editors and scripts)")
@click.option("--report", type=click.Path(path_type=Path), help="Write run metrics JSON here")
@click.option("--add-dir", "add_dirs", multiple=True, type=click.Path(exists=True, file_okay=False, path_type=Path), help="Extra directory the agent may use besides the working directory (repeatable)")
@click.option("--resume", "resume_id", help="Continue a saved session by id (shown when you exit; /sessions lists them)")
@click.option("--continue", "continue_last", is_flag=True, help="Continue the newest saved session that ran in this folder")
@click.option("--trust", "trust_project", is_flag=True, help="Trust this project's .ai-agent/config.toml (hooks, verify commands, approval)")
@click.option("--challenge", "challenge_mode", is_flag=True, help="Have a second model argue against risky requests first (always on interactively)")
@click.option("--effort", type=click.Choice(["low", "medium", "high"]), help="Reasoning effort (Codex models)")
@click.option("--model", "-m", help="Model id (overrides config)")
@click.option(
    "--approval",
    type=click.Choice([p.value for p in ApprovalPolicy]),
    help="Approval policy (overrides config)",
)
def main(
    prompt: str | None,
    cwd: Path | None,
    add_dirs: tuple,
    trust_project: bool,
    resume_id: str | None,
    continue_last: bool,
    parallel: bool,
    graph: Path | None,
    apply: bool,
    json_mode: bool,
    rpc_mode: bool,
    report: Path | None,
    challenge_mode: bool,
    effort: str | None,
    model: str | None,
    approval: str | None,
):
    try:
        config = load_config(cwd=cwd, trust_project=trust_project)
        if config.ignored_project_keys:
            if sys.stdin.isatty() and sys.stdout.isatty() and not prompt:
                console.print(f"[warning]This project's .ai-agent/config.toml sets: {', '.join(config.ignored_project_keys)}[/warning]")
                console.print("[dim]These can run commands or relax approvals. They are ignored unless you trust the file.[/dim]")
                if input("Trust it? [y/N] ").strip().lower() in {"y", "yes"}:
                    config = load_config(cwd=cwd, trust_project=True)
            else:
                console.print(f"[warning]Ignoring untrusted project settings: {', '.join(config.ignored_project_keys)} (use --trust to enable)[/warning]")
    except Exception as e:
        console.print(f"[error]Configuration Error: {e}[/error]")
        sys.exit(1)

    if add_dirs:
        config.allowed_dirs = [*config.allowed_dirs, *(str(d.resolve()) for d in add_dirs)]
    # Interactive only: start on the model used last time. One-shot runs (benchmarks)
    # keep the configured routing and never touch the remembered model.
    if not prompt:
        model = model or load_last_model()
    if model:
        config.model_name = model
        config.model_pinned = True
        if not prompt:
            save_last_model(model)
    if effort:
        config.reasoning_effort = effort
    config.auto_challenge = challenge_mode or not (prompt or json_mode or rpc_mode)
    if approval:
        config.approval = ApprovalPolicy(approval)
    elif prompt and config.approval == ApprovalPolicy.ON_REQUEST:
        # A one-shot run has nobody to ask, so the default would refuse every edit.
        # Let it edit files in the workspace; commands still need an explicit --approval auto.
        # ponytail: an explicit `approval = "on-request"` in config is treated like the default.
        config.approval = ApprovalPolicy.AUTO_EDIT

    errors = config.validate()

    if errors:
        for error in errors:
            console.print(f"[error]{error}[/error]")

        sys.exit(1)

    cli = CLI(config)
    if continue_last and not resume_id:
        resume_id = PersistenceManager().latest_for(str(config.cwd))
        if not resume_id:
            console.print("[dim]No saved session for this folder; starting a new one.[/dim]")
    cli.resume_id = resume_id

    if (parallel or graph) and prompt:
        ok = asyncio.run(cli.run_orchestrated(prompt, graph, apply, report))
        sys.exit(0 if ok else 1)

    if rpc_mode:
        asyncio.run(cli.run_rpc())
        sys.exit(0)
    if json_mode:
        if not prompt:
            console.print("[error]--json needs a prompt[/error]")
            sys.exit(1)
        sys.exit(0 if asyncio.run(cli.run_json(prompt)) else 1)

    if prompt:
        result = asyncio.run(cli.run_single(prompt, report))
        bad = {"failed", "verification_failed"}
        if result is None or (cli.last_report or {}).get("status") in bad:
            sys.exit(1)
    else:
        asyncio.run(cli.run_interactive())


if __name__ == "__main__":
    main()
