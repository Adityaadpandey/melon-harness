"""Terminal rendering with mew's own look: a night-sky palette (pink, lavender, mint),
coloured chips for each tool call, a rail down the left of every block, syntax-highlighted
diffs with tinted rows, and a shimmering status line while the agent works."""
from __future__ import annotations

import math
import re
import time
from collections import deque
from pathlib import Path
from typing import Any

from rich import box
from rich.console import Console, ConsoleOptions, Group, RenderResult
from rich.live import Live
from rich.padding import Padding
from rich.table import Table
from rich.markdown import Markdown
from rich.segment import Segment
from rich.syntax import Syntax
from rich.panel import Panel
from rich.text import Text
from rich.theme import Theme

from config.config import Config
from safety.approval import Decision
from tools.base import ToolConfirmation
from utils.paths import display_path_rel_to_cwd

ACCENT = "#ff7eb6"    # mew pink
LAVENDER = "#b49cff"
MINT = "#6ee7b7"
AMBER = "#fbbf6a"
SKY = "#7cc4ff"
TEAL = "#5eead4"
CORAL = "#ff6b7f"
FG = "#d5d9ec"
MUTED = "#6c7393"
INK = "#14161f"       # text on top of a coloured chip

AGENT_THEME = Theme(
    {
        "accent": ACCENT,
        "info": SKY,
        "warning": AMBER,
        "error": f"bold {CORAL}",
        "success": MINT,
        "dim": MUTED,
        "muted": MUTED,
        "border": "#3a3f5c",
        "highlight": f"bold {ACCENT}",
        "user": f"bold {ACCENT}",
        "assistant": FG,
        "tool": "bold",
        "tool.read": SKY,
        "tool.write": AMBER,
        "tool.shell": LAVENDER,
        "tool.network": TEAL,
        "tool.memory": MINT,
        "tool.mcp": SKY,
        "code": FG,
        "diff.add": MINT,
        "diff.del": CORAL,
        # markdown: headings in the palette, inline code as a tinted pill
        "markdown.h1": f"bold {ACCENT}",
        "markdown.h2": f"bold {LAVENDER}",
        "markdown.h3": f"bold {SKY}",
        "markdown.h4": f"bold {TEAL}",
        "markdown.code": f"{AMBER} on #232640",
        "markdown.strong": "bold #f4f5ff",
        "markdown.em": "italic #c9cdea",
        "markdown.link": f"underline {SKY}",
        "markdown.link_url": MUTED,
        "markdown.item.bullet": f"bold {ACCENT}",
        "markdown.item.number": f"bold {ACCENT}",
        "markdown.block_quote": f"italic #8f96b8",
        "markdown.hr": "#3a3f5c",
    }
)

_console: Console | None = None


def get_console() -> Console:
    global _console
    if _console is None:
        _console = Console(theme=AGENT_THEME, highlight=False)
    return _console


# ---------------------------------------------------------------- formatting

TITLES = {
    "read_file": "Read", "write_file": "Write", "edit": "Update", "shell": "Bash", "grep": "Search",
    "glob": "Find", "list_dir": "List", "web_search": "Web Search", "web_fetch": "Fetch",
    "todos": "Todos", "memory": "Memory", "use_skill": "Skill",
}
PRIMARY_ARG = {
    "read_file": "path", "write_file": "path", "edit": "path", "shell": "command", "grep": "pattern",
    "glob": "pattern", "list_dir": "path", "web_search": "query", "web_fetch": "url", "use_skill": "name",
}


def tool_label(name: str, args: dict[str, Any], cwd: Path | None) -> str:
    if name.startswith("subagent_"):
        goal = str(args.get("goal", ""))
        return f"Agent[{name[len('subagent_'):]}]({goal[:70]}{'…' if len(goal) > 70 else ''})"
    key = PRIMARY_ARG.get(name)
    val = args.get(key) if key else None
    if val is None and args:
        val = next(iter(args.values()))
    val = "" if val is None else str(val)
    if key == "path" and cwd:
        val = display_path_rel_to_cwd(val, cwd)
    val = val.replace("\n", " ")
    if len(val) > 90:
        val = val[:89] + "…"
    extra = f", {args['glob']}" if name == "grep" and args.get("glob") else ""
    return f"{TITLES.get(name, name)}({val}{extra})"


def _first_lines(text: str, n: int) -> tuple[list[str], int]:
    lines = text.splitlines()
    return lines[:n], max(0, len(lines) - n)


def parse_unified_diff(diff: str) -> tuple[list[tuple[str, int | None, str]], int, int]:
    """[(kind, line_no, text)] with kind in '+', '-', ' ', plus add/remove counts."""
    rows: list[tuple[str, int | None, str]] = []
    adds = dels = old_n = new_n = 0
    for line in diff.splitlines():
        if line.startswith(("---", "+++")):
            continue
        if line.startswith("@@"):
            m = re.match(r"@@ -(\d+)(?:,\d+)? \+(\d+)", line)
            if m:
                old_n, new_n = int(m.group(1)), int(m.group(2))
        elif line.startswith("+"):
            adds += 1
            rows.append(("+", new_n, line[1:]))
            new_n += 1
        elif line.startswith("-"):
            dels += 1
            rows.append(("-", old_n, line[1:]))
            old_n += 1
        else:
            rows.append((" ", new_n, line[1:] if line.startswith(" ") else line))
            old_n += 1
            new_n += 1
    return rows, adds, dels


def render_diff(diff: str, max_lines: int = 16) -> tuple[Text, int, int]:
    rows, adds, dels = parse_unified_diff(diff)
    out = Text()
    shown = shown_changed = 0
    for kind, no, txt in rows:
        if shown >= max_lines:
            break
        style = {"+": "diff.add", "-": "diff.del"}.get(kind, "dim")
        out.append(f"    {no if no is not None else '':>5} {kind} {txt[:160]}\n", style=style)
        shown += 1
        shown_changed += kind in "+-"
    hidden = adds + dels - shown_changed
    if hidden > 0:
        out.append(f"          … +{hidden} more changed lines\n", style="dim")
    return out, adds, dels


def summarize_result(name: str, success: bool, output: str, error: str | None, metadata: dict,
                     exit_code: int | None, args: dict) -> tuple[str, list[str]]:
    """(one-line summary, preview lines) for the ⎿ block."""
    md = metadata or {}
    if not success:
        lines, _ = _first_lines((error or output or "failed").strip(), 3)
        return f"✗ {lines[0] if lines else 'failed'}", lines[1:]
    if name == "read_file":
        if md.get("deduped"):
            return "Already read (unchanged)", []
        total, a, b = md.get("total_lines"), md.get("shown_start"), md.get("shown_end")
        n = (b - a + 1) if a and b else total
        return f"Read {n if n is not None else '?'} lines" + (f" (of {total})" if total and n != total else ""), []
    if name in {"edit", "write_file"}:
        return (output.strip().splitlines() or ["Done"])[0], []
    if name == "shell":
        lines, more = _first_lines(output.strip(), 6) if output.strip() else ([], 0)
        tag = f"exit {exit_code}" if exit_code not in (None, 0) else "Ran"
        return tag, lines + ([f"… +{more} lines"] if more else [])
    if name == "grep":
        m, f = md.get("matches"), md.get("files_with_matches")
        head = f"Found {m} match{'es' if m != 1 else ''} in {f} file{'s' if f != 1 else ''}" if m else "No matches"
        lines, more = _first_lines(output, 4) if m else ([], 0)
        return head, lines + ([f"… +{more} lines"] if more else [])
    if name == "glob":
        m = md.get("matches", 0)
        lines, more = _first_lines(output, 4) if m else ([], 0)
        return (f"Found {m} file{'s' if m != 1 else ''}" if m else "No files"), lines + ([f"… +{more} lines"] if more else [])
    if name == "list_dir":
        return f"Listed {md.get('entries', 0)} entries", []
    if name == "use_skill":
        return f"Loaded {args.get('name', 'skill')}" + (f" · {args['section']}" if args.get("section") else ""), []
    if name.startswith("subagent_"):
        lines, more = _first_lines(output.split("Result:", 1)[-1].strip(), 4)
        return "Done", lines + ([f"… +{more} lines"] if more else [])
    lines, more = _first_lines(output.strip(), 3)
    return (lines[0] if lines else "Done"), lines[1:] + ([f"… +{more} lines"] if more else [])


# ------------------------------------------------------------ streaming text

_CONTINUES_BLOCK = re.compile(r"^(\s|[-*+]\s|\d+[.)]\s|>|\|)")


def split_blocks(buf: str) -> tuple[str, str]:
    """(finished markdown blocks, unfinished tail). A block ends at a blank line outside a code
    fence, unless the next line continues a list, quote, table or indented block, so numbered
    lists and fenced code never get cut in half while text is still streaming."""
    lines = buf.split("\n")
    in_fence, has_content, cut, pos = False, False, 0, 0
    for i, line in enumerate(lines[:-1]):  # the last element is a partial line
        pos += len(line) + 1
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            has_content = True
        elif in_fence:
            continue
        elif line.strip():
            has_content = True
        elif has_content:
            nxt = next((ln for ln in lines[i + 1:] if ln.strip() or ln == lines[-1]), None)
            if nxt is None or not nxt:
                continue  # nothing after the blank line yet: wait
            if not _CONTINUES_BLOCK.match(nxt):
                cut, has_content = pos, False
    return buf[:cut], buf[cut:]


def context_bar(pct: float, width: int = 10) -> str:
    filled = max(0, min(width, round(pct / 100 * width)))
    return "▰" * filled + "▱" * (width - filled) + f" {pct:.0f}%"


SPIN = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
TIPS = [
    "@path attaches a file · Tab completes",
    "!git status runs a command and shows it to the model",
    "/compact frees context when a long session slows down",
    "/cost shows tokens, cache hits and why a cache missed",
    "Alt+Enter inserts a newline",
    "Several read-only sub-agents in one turn run in parallel",
]


class TUI:
    def __init__(self, config: Config, console: Console | None = None) -> None:
        self.console = console or get_console()
        self.config = config
        self.cwd = config.cwd
        self._calls: dict[str, tuple[str, dict[str, Any]]] = {}
        self._header_done = False  # an approval prompt already printed the call's header
        self.expand = False
        # live turn state
        self.live: Live | None = None
        self._turn_active = False
        self._turn_started = 0.0
        self._buf = ""
        self._first_block = True
        self._out_chars = 0
        self._phase = "Thinking"
        self.todos: list[tuple[str, bool]] = []
        self.agents: dict[str, dict[str, Any]] = {}
        self.results: deque = deque(maxlen=30)  # full tool results for /expand

    # -- live region -----------------------------------------------------
    def _live_ok(self) -> bool:
        return self.console.is_terminal and not getattr(self.console, "record", False)

    def _renderable(self):
        rows: list[Any] = []
        if self._buf.strip():
            tail = self._buf if self._buf.count("\n") < 14 else "…\n" + "\n".join(self._buf.split("\n")[-12:])
            rows.append(self._assistant_grid(tail, self._first_block))
        elapsed = int(time.monotonic() - self._turn_started)
        frame = SPIN[int(time.monotonic() * 10) % len(SPIN)]
        tokens = f" · ↓ ~{self._out_chars / 4000:.1f}k tokens" if self._out_chars > 400 else ""
        rows.append(Text.assemble((f"{frame} ", ACCENT), (f"{self._phase}…", ACCENT), (f"  ({elapsed}s{tokens} · ctrl+c to interrupt)", "dim")))
        for a in self.agents.values():
            rows.append(Text(f"  ⎿ {a['agent']} · {a['last']} · {int(time.monotonic() - a['started'])}s", style="dim"))
        if self.todos:
            rows.append(Text("  Tasks", style="dim"))
            for content, done in self.todos[:8]:
                rows.append(Text(f"   {'☒' if done else '☐'} {content[:90]}", style="dim strike" if done else "default"))
        return Group(*rows)

    def _start_live(self) -> None:
        if self._live_ok() and self.live is None:
            self.live = Live(get_renderable=self._renderable, console=self.console, refresh_per_second=12, transient=True)
            self.live.start()

    def _stop_live(self) -> None:
        if self.live is not None:
            self.live.stop()
            self.live = None

    def start_turn(self, phase: str = "Thinking") -> None:
        self._turn_active = True
        self._turn_started = time.monotonic()
        self._out_chars, self._buf, self._first_block, self._phase = 0, "", True, phase
        self._start_live()

    def end_turn(self) -> None:
        self.assistant_flush()
        self._turn_active = False
        self._stop_live()
        self.agents.clear()

    def pause(self) -> None:
        """Stop the live region while something else needs the terminal (approval prompt)."""
        self._stop_live()

    def resume(self) -> None:
        if self._turn_active:
            self._start_live()

    def set_phase(self, phase: str) -> None:
        self._phase = phase

    # kept for callers that start/stop a bare spinner (e.g. /compact)
    def start_spinner(self, label: str = "Working") -> None:
        self._phase = label
        if not self._turn_active:
            self._turn_started = time.monotonic()
            self._turn_active = True
        self._start_live()

    def stop_spinner(self) -> None:
        if self._turn_active and self.live is not None and not self._buf:
            pass  # the live region persists for the turn; end_turn() closes it

    # -- transcript ----------------------------------------------------
    def print_welcome(self, title: str, lines: list[str]) -> None:
        import random

        body = Text()
        body.append("✻ ", style=ACCENT)
        body.append(f"Welcome to {title}\n\n", style="bold")
        for ln in lines:
            body.append(f"  {ln}\n", style="dim")
        body.append(f"\n  tip: {random.choice(TIPS)}", style="dim italic")
        self.console.print(Panel(body, border_style=ACCENT, box=box.ROUNDED, padding=(0, 2), expand=False))

    def echo_user(self, text: str) -> None:
        first, *rest = text.strip().split("\n")
        shown = first + (f"  (+{len(rest)} lines)" if rest else "")
        self.console.print(Padding(Text.assemble(("❯ ", f"bold {ACCENT}"), (shown[:300], "default")), (0, 1), style="on #262626"))

    def _assistant_grid(self, md: str, first: bool):
        grid = Table.grid(padding=(0, 1))
        grid.add_column(width=1)
        grid.add_column(ratio=1)
        grid.add_row(Text("⏺" if first else " ", style=ACCENT), Markdown(md.rstrip(), code_theme="monokai"))
        return grid

    def _print_block(self, md: str) -> None:
        if not md.strip():
            return
        self.console.print()
        self.console.print(self._assistant_grid(md, self._first_block))
        self._first_block = False

    def assistant_delta(self, content: str) -> None:
        self._buf += content
        self._out_chars += len(content)
        self._phase = "Writing"
        done, rest = split_blocks(self._buf)
        if done:
            self._print_block(done)
            self._buf = rest

    def assistant_flush(self) -> None:
        if self._buf.strip():
            self._print_block(self._buf)
        self._buf = ""
        self._first_block = True  # the next piece of text (after a tool call) starts a new message

    def notice(self, message: str) -> None:
        self.console.print(Text(f"  ✻ {message}", style="dim italic"))

    def error(self, message: str) -> None:
        self.assistant_flush()
        self.console.print(Text(f"\n✗ {message}", style="error"))

    def set_todos(self, todos: list[tuple[str, bool]]) -> None:
        self.todos = todos

    def agent_progress(self, call_id: str, info: dict[str, Any]) -> None:
        kind = info.get("kind")
        if kind == "start":
            self.agents[call_id] = {"agent": info["agent"], "last": "starting", "started": time.monotonic()}
        elif kind == "tool" and call_id in self.agents:
            self.agents[call_id]["last"] = tool_label(info.get("name", ""), info.get("args", {}), self.cwd)
        elif kind == "done":
            self.agents.pop(call_id, None)

    def tool_call_start(self, call_id: str, name: str, tool_kind: str | None, arguments: dict[str, Any]) -> None:
        """Only remembers the call. The header is printed together with its result
        (or with the approval prompt), so parallel calls never interleave."""
        self.assistant_flush()
        self._calls[call_id] = (name, arguments)
        self._phase = f"Running {tool_label(name, arguments, self.cwd)}" if len(self._calls) == 1 else f"Running {len(self._calls)} tools"

    def _print_header(self, name: str, args: dict[str, Any]) -> None:
        self.console.print()
        self.console.print(Text.assemble(("⏺ ", ACCENT), (tool_label(name, args, self.cwd), "bold")))

    def tool_call_complete(self, call_id: str, name: str, tool_kind: str | None, success: bool, output: str,
                           error: str | None, metadata: dict[str, Any] | None, diff: str | None,
                           truncated: bool, exit_code: int | None) -> None:
        args = self._calls.pop(call_id, (name, {}))[1]
        self._phase = "Thinking"
        if self._header_done:
            self._header_done = False
        else:
            self._print_header(name, args)
        md = metadata or {}
        self.results.append({"label": tool_label(name, args, self.cwd), "success": success, "output": output, "error": error, "diff": diff})
        summary, preview = summarize_result(name, success, output, error, md, exit_code, args)
        style = "error" if not success else "dim"
        if name in {"edit", "write_file"} and success and diff:
            body, adds, dels = render_diff(diff, 400 if self.expand else 14)
            path = display_path_rel_to_cwd(md.get("path", args.get("path", "")), self.cwd)
            plus = f"{adds} addition{'s' if adds != 1 else ''}" if adds else ""
            minus = f"{dels} removal{'s' if dels != 1 else ''}" if dels else ""
            what = " and ".join(x for x in (plus, minus) if x) or "no line changes"
            verb = "Created" if md.get("is_new_file") else "Updated"
            self.console.print(Text.assemble(("  ⎿  ", "dim"), (f"{verb} {path} with {what}", style)))
            self.console.print(body, end="")
            return
        self.console.print(Text.assemble(("  ⎿  ", "dim"), (summary, style)))
        for ln in preview[: 60 if self.expand else None]:
            self.console.print(Text(f"     {ln[:200]}", style="dim"))

    def show_result(self, n: int = 1) -> None:
        """/expand [n]: print the full output of the n-th most recent tool call."""
        if not self.results or n < 1 or n > len(self.results):
            self.console.print("[dim]  no such tool result[/dim]")
            return
        r = list(self.results)[-n]
        if r["diff"]:
            body: Any = render_diff(r["diff"], 1000)[0]
        else:
            text = (r["error"] or "") + ("\n" if r["error"] and r["output"] else "") + (r["output"] or "")
            lines = text.splitlines()
            body = Syntax("\n".join(lines[:300]) + (f"\n… +{len(lines) - 300} lines" if len(lines) > 300 else ""), "text", word_wrap=True, theme="monokai")
        self.console.print(Panel(body, title=Text(r["label"], style="bold"), title_align="left", border_style="border", box=box.ROUNDED))

    def turn_summary(self, elapsed: float, usage: dict | None, report: dict | None) -> None:
        bits = [f"{elapsed:.0f}s"]
        if usage and usage.get("calls"):
            bits += [f"{usage['calls']} calls", f"{usage['prompt_tokens'] / 1000:.1f}k in ({usage['cache_hit_pct']:.0f}% cached)", f"{usage['completion_tokens'] / 1000:.1f}k out"]
            if usage.get("est_cost_usd") is not None:
                bits.append(f"${usage['est_cost_usd']}")
        self.console.print(Text("  ✻ " + " · ".join(bits), style="dim"))

    # -- approval ------------------------------------------------------
    async def confirm(self, confirmation: ToolConfirmation) -> Decision:
        from ui.prompt import ask_choice, read_line

        self.pause()
        try:
            if confirmation.command:
                parts: list[Any] = [Text(f"$ {confirmation.command}", style="warning")]
                title, q = "Run command", "Do you want to run this command?"
                always = "Yes, and don't ask again for this kind of command this session"
            elif confirmation.diff:
                parts = [render_diff(confirmation.diff.to_diff(), 40)[0]]
                title, q = confirmation.description, "Do you want to make this edit?"
                always = "Yes, and don't ask again for edits this session"
            else:
                parts = [Text(confirmation.description)]
                title, q, always = confirmation.tool_name, "Do you want to proceed?", "Yes, and don't ask again this session"
            self._print_header(confirmation.tool_name, confirmation.params)
            self._header_done = True
            self.console.print(Panel(Group(*parts), title=Text(title.split("\n")[0], style="bold"), title_align="left",
                                     border_style=ACCENT, box=box.ROUNDED, padding=(0, 1)))
            choice = await ask_choice(self.console, q, ["Yes", always, "No, and tell me what to do differently"])
            if choice == 0:
                return Decision(True)
            if choice == 1:
                return Decision(True, always=True)
            feedback = await read_line("What should I do instead? (Enter to skip) ")
            return Decision(False, feedback=f"The user said: {feedback}" if feedback else None)
        finally:
            self.resume()

    handle_confirmation = confirm

    # -- help ----------------------------------------------------------
    def show_help(self) -> None:
        from ui.prompt import COMMANDS

        rows = "\n".join(f"- `{c}`  {d}" for c, d in COMMANDS.items())
        self.console.print(Markdown(
            "## Commands\n" + rows
            + "\n\n## Input\n- `@path` attaches a file (Tab completes)\n"
            "- `!cmd` runs a shell command and shows the output to the model; `!!cmd` runs it privately\n"
            "- `Alt+Enter` inserts a newline · `Ctrl+C` interrupts the agent\n"
        ))
