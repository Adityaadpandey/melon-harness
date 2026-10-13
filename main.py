import asyncio
import json
from pathlib import Path
import sys
import click

from agent.agent import Agent
from agent.events import AgentEventType
from agent.persistence import PersistenceManager, SessionSnapshot
from agent.session import Session
from config.config import ApprovalPolicy, Config
from config.loader import load_config
from ui.tui import TUI, get_console

console = get_console()


class CLI:
    def __init__(self, config: Config):
        self.agent: Agent | None = None
        self.config = config
        self.tui = TUI(config, console)
        self.last_report: dict | None = None

    async def run_single(self, message: str, report_path: Path | None = None) -> str | None:
        async with Agent(self.config) as agent:
            self.agent = agent
            result = await self._process_message(message)
            if report_path:
                self._write_report(report_path, agent, result)
            return result

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

    async def run_interactive(self) -> str | None:
        self.tui.print_welcome(
            "AI Agent",
            lines=[
                f"model: {self.config.model_name}",
                f"cwd: {self.config.cwd}",
                "commands: /help /config /approval /model /exit",
            ],
        )

        async with Agent(
            self.config,
            confirmation_callback=self.tui.handle_confirmation,
        ) as agent:
            self.agent = agent

            while True:
                try:
                    user_input = console.input("\n[user]>[/user] ").strip()
                    if not user_input:
                        continue

                    if user_input.startswith("/"):
                        should_continue = await self._handle_command(user_input)
                        if not should_continue:
                            break
                        continue

                    await self._process_message(user_input)
                except KeyboardInterrupt:
                    console.print("\n[dim]Use /exit to quit[/dim]")
                except EOFError:
                    break

        console.print("\n[dim]Goodbye![/dim]")

    def _print_report(self, report: dict | None) -> None:
        if not report:
            return
        u = report.get("usage") or {}
        if u.get("calls"):
            cost = f"  est ${u['est_cost_usd']}" if u.get("est_cost_usd") is not None else ""
            console.print(
                f"\n[dim]usage: {u['calls']} calls, {u['prompt_tokens']:,} in "
                f"({u['cache_hit_pct']}% cached), {u['completion_tokens']:,} out{cost}[/dim]"
            )
        if report["status"] == "done_no_changes":
            return
        style = {"verified": "success", "unverified": "warning"}.get(report["status"], "error")
        console.print(f"\n[{style}]Result: {report['status']}[/{style}]  "
                      f"files changed: {len(report['changed_files'])}  repairs: {report['repairs']}")
        for c in report["checks"]:
            mark = "ok" if c["ok"] else "FAILED"
            console.print(f"  [{mark}] {c['command']} ({c['seconds']}s)")
        if report["status"] == "unverified":
            console.print("  [dim]No verification commands found; changes were not tested.[/dim]")

    def _get_tool_kind(self, tool_name: str) -> str | None:
        tool_kind = None
        tool = self.agent.session.tool_registry.get(tool_name)
        return tool.kind.value if tool else None

    async def _process_message(self, message: str) -> str | None:
        if not self.agent:
            return None

        assistant_streaming = False
        final_response: str | None = None

        async for event in self.agent.run(message):
            if event.type == AgentEventType.TEXT_DELTA:
                content = event.data.get("content", "")
                if not assistant_streaming:
                    self.tui.begin_assistant()
                    assistant_streaming = True
                self.tui.stream_assistant_delta(content)
            elif event.type == AgentEventType.TEXT_COMPLETE:
                final_response = event.data.get("content")
                if assistant_streaming:
                    self.tui.end_assistant()
                    assistant_streaming = False
            elif event.type == AgentEventType.AGENT_END:
                self.last_report = event.data.get("report")
                self._print_report(self.last_report)
            elif event.type == AgentEventType.NOTICE:
                console.print(f"[warning]{event.data.get('message')}[/warning]")
            elif event.type == AgentEventType.AGENT_ERROR:
                error = event.data.get("error", "Unknown error")
                console.print(f"\n[error]Error: {error}[/error]")
            elif event.type == AgentEventType.TOOL_CALL_START:
                tool_name = event.data.get("name", "unknown")
                tool_kind = self._get_tool_kind(tool_name)
                self.tui.tool_call_start(
                    event.data.get("call_id", ""),
                    tool_name,
                    tool_kind,
                    event.data.get("arguments", {}),
                )
            elif event.type == AgentEventType.TOOL_CALL_COMPLETE:
                tool_name = event.data.get("name", "unknown")
                tool_kind = self._get_tool_kind(tool_name)
                self.tui.tool_call_complete(
                    event.data.get("call_id", ""),
                    tool_name,
                    tool_kind,
                    event.data.get("success", False),
                    event.data.get("output", ""),
                    event.data.get("error"),
                    event.data.get("metadata"),
                    event.data.get("diff"),
                    event.data.get("truncated", False),
                    event.data.get("exit_code"),
                )

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
                    console.print(f"[success]Model changed to: {cmd_args} [/success]")
            else:
                console.print(f"Current model: {self.config.model_name}")
        elif cmd_name == "/approval":
            if cmd_args:
                try:
                    approval = ApprovalPolicy(cmd_args)
                    self.config.approval = approval
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
                console.print(
                    f"  • {s['session_id']} (turns: {s['turn_count']}, updated: {s['updated_at']})"
                )
        elif cmd_name == "/resume":
            if not cmd_args:
                console.print(f"[error]Usage: /resume <session_id> [/error]")
            else:
                persistence_manager = PersistenceManager()
                snapshot = persistence_manager.load_session(cmd_args)
                if not snapshot:
                    console.print(f"[error]Session does not exist [/error]")
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
                        f"[success]Resumed session: {session.session_id}[/success]"
                    )
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
@click.option("--report", type=click.Path(path_type=Path), help="Write run metrics JSON here")
@click.option("--model", "-m", help="Model id (overrides config)")
@click.option(
    "--approval",
    type=click.Choice([p.value for p in ApprovalPolicy]),
    help="Approval policy (overrides config)",
)
def main(
    prompt: str | None,
    cwd: Path | None,
    parallel: bool,
    graph: Path | None,
    apply: bool,
    report: Path | None,
    model: str | None,
    approval: str | None,
):
    try:
        config = load_config(cwd=cwd)
    except Exception as e:
        console.print(f"[error]Configuration Error: {e}[/error]")
        sys.exit(1)

    if model:
        config.model_name = model
    if approval:
        config.approval = ApprovalPolicy(approval)

    errors = config.validate()

    if errors:
        for error in errors:
            console.print(f"[error]{error}[/error]")

        sys.exit(1)

    cli = CLI(config)

    if (parallel or graph) and prompt:
        ok = asyncio.run(cli.run_orchestrated(prompt, graph, apply, report))
        sys.exit(0 if ok else 1)

    # messages = [{"role": "user", "content": prompt}]
    if prompt:
        result = asyncio.run(cli.run_single(prompt, report))
        bad = {"failed", "verification_failed"}
        if result is None or (cli.last_report or {}).get("status") in bad:
            sys.exit(1)
    else:
        asyncio.run(cli.run_interactive())


if __name__ == "__main__":
    main()
