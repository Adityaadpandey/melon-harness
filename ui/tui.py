"""Terminal rendering in the style of Claude Code: a `⏺ Tool(args)` line per
call, a `⎿` result summary under it, compact numbered diffs for edits, one
spinner line with elapsed time, and a single-keypress approval prompt."""
from __future__ import annotations

import re
import time
from pathlib import Path
from typing import Any

from rich import box
from rich.console import Console, Group
from rich.markdown import Markdown
from rich.panel import Panel
from rich.text import Text
from rich.theme import Theme

from config.config import Config
from safety.approval import Decision
from tools.base import ToolConfirmation
from utils.paths import display_path_rel_to_cwd

ACCENT = "#d97757"  # Claude orange

AGENT_THEME = Theme(
    {
        "accent": ACCENT,
        "info": "cyan",
        "warning": "yellow",
        "error": "bright_red",
        "success": "green",
        "dim": "dim",
        "muted": "grey50",
        "border": "grey35",
        "highlight": f"bold {ACCENT}",
        "user": f"bold {ACCENT}",
        "assistant": "default",
        "tool": "bold",
        "tool.read": "cyan",
        "tool.write": "yellow",
        "tool.shell": "magenta",
        "tool.network": "bright_blue",
        "tool.memory": "green",
        "tool.mcp": "bright_cyan",
        "code": "white",
        "diff.add": "green",
        "diff.del": "red",
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


# ------------------------------------------------------------------------- UI

class TUI:
    def __init__(self, config: Config, console: Console | None = None) -> None:
        self.console = console or get_console()
        self.config = config
        self.cwd = config.cwd
        self._assistant_open = False
        self._calls: dict[str, tuple[str, dict[str, Any]]] = {}
        self._header_done = False  # an approval prompt already printed the call's header
        self._status = None
        self.expand = False  # /expand toggles longer tool output

    # -- spinner -------------------------------------------------------
    def start_spinner(self, label: str = "Working") -> None:
        if self._status is not None:
            return
        self._status = self.console.status(
            Text.assemble((f"{label}…", ACCENT), ("  (ctrl+c to interrupt)", "dim")),
            spinner="dots", spinner_style=ACCENT,
        )
        self._status.start()

    def stop_spinner(self) -> None:
        if self._status is not None:
            self._status.stop()
            self._status = None

    # -- transcript ----------------------------------------------------
    def print_welcome(self, title: str, lines: list[str]) -> None:
        body = Text()
        body.append("✻ ", style=ACCENT)
        body.append(f"Welcome to {title}\n\n", style="bold")
        for ln in lines:
            body.append(f"  {ln}\n", style="dim")
        self.console.print(Panel(body, border_style=ACCENT, box=box.ROUNDED, padding=(0, 2), expand=False))

    def begin_assistant(self) -> None:
        self.stop_spinner()
        self.console.print()
        self.console.print(Text("⏺ ", style=ACCENT), end="")
        self._assistant_open = True

    def stream_assistant_delta(self, content: str) -> None:
        self.console.print(content, end="", markup=False, soft_wrap=True)

    def end_assistant(self) -> None:
        if self._assistant_open:
            self.console.print()
        self._assistant_open = False

    def notice(self, message: str) -> None:
        self.stop_spinner()
        self.console.print(Text(f"  ✻ {message}", style="dim italic"))

    def error(self, message: str) -> None:
        self.stop_spinner()
        self.console.print(Text(f"\n✗ {message}", style="error"))

    def tool_call_start(self, call_id: str, name: str, tool_kind: str | None, arguments: dict[str, Any]) -> None:
        """Only remembers the call. The header is printed together with its result
        (or with the approval prompt), so parallel calls never interleave."""
        self._calls[call_id] = (name, arguments)
        if self._assistant_open:
            self.end_assistant()
        self.stop_spinner()
        self.start_spinner(f"Running {tool_label(name, arguments, self.cwd)}")

    def _print_header(self, name: str, args: dict[str, Any]) -> None:
        self.console.print()
        self.console.print(Text.assemble(("⏺ ", ACCENT), (tool_label(name, args, self.cwd), "bold")))

    def tool_call_complete(self, call_id: str, name: str, tool_kind: str | None, success: bool, output: str,
                           error: str | None, metadata: dict[str, Any] | None, diff: str | None,
                           truncated: bool, exit_code: int | None) -> None:
        args = self._calls.pop(call_id, (name, {}))[1]
        self.stop_spinner()
        if self._header_done:
            self._header_done = False
        else:
            self._print_header(name, args)
        md = metadata or {}
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

    # -- approval ------------------------------------------------------
    async def confirm(self, confirmation: ToolConfirmation) -> Decision:
        from ui.prompt import ask_choice, read_line

        self.stop_spinner()
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
        self.console.print(Panel(Group(*parts), title=Text(title, style="bold"), title_align="left",
                                 border_style=ACCENT, box=box.ROUNDED, padding=(0, 1)))
        choice = await ask_choice(self.console, q, ["Yes", always, "No, and tell me what to do differently"])
        if choice == 0:
            return Decision(True)
        if choice == 1:
            return Decision(True, always=True)
        feedback = await read_line("What should I do instead? (Enter to skip) ")
        return Decision(False, feedback=f"The user said: {feedback}" if feedback else None)

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
