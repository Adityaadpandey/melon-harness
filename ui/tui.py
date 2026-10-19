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
from rich.markdown import Heading, Markdown
from rich.segment import Segment
from rich.syntax import Syntax
from rich.panel import Panel
from rich.style import Style
from rich.text import Span, Text
from rich.theme import Theme

from config.config import Config
from safety.approval import Decision
from tools.base import ToolConfirmation
from utils.paths import display_path_rel_to_cwd

from ui.palette import ACCENT, AMBER, CORAL, FG, INK, LAVENDER, MINT, MUTED, SKY, TEAL, BG_BOX, FAINT, RULE

AGENT_THEME = Theme(
    {
        "accent": ACCENT,
        "info": SKY,
        "warning": AMBER,
        "error": f"bold {CORAL}",
        "success": MINT,
        "dim": MUTED,
        "muted": MUTED,
        "border": RULE,
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
        "markdown.code": f"{AMBER} on #2a2723",
        "markdown.strong": "bold #f6f1e4",
        "markdown.em": "italic #cfc8b8",
        "markdown.link": f"underline {SKY}",
        "markdown.link_url": MUTED,
        "markdown.item.bullet": f"bold {ACCENT}",
        "markdown.item.number": f"bold {ACCENT}",
        "markdown.block_quote": f"italic #a39c8c",
        "markdown.hr": RULE,
    }
)

_console: Console | None = None

# shell commands that only look around; the side panel skips them like it skips grep
QUIET_COMMANDS = {"grep", "rg", "find", "ls", "cat", "head", "tail", "wc", "tree", "pwd", "echo", "fd", "which"}


def redirect_console(console: Console, sink: Any, width: int) -> None:
    """Point the shared console at a full-screen pane. It keeps emitting colour codes even though
    its file is not a terminal, and wraps to the pane's width."""
    from rich.color import ColorSystem

    console.file = sink
    console._force_terminal = True
    console._color_system = ColorSystem.TRUECOLOR
    console.width = width


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


KIND_COLOR = {
    "read_file": SKY, "glob": SKY, "grep": SKY, "list_dir": SKY, "search": SKY, "graph": SKY,
    "edit": AMBER, "write_file": AMBER,
    "shell": LAVENDER,
    "web_search": TEAL, "web_fetch": TEAL,
    "memory": MINT, "todos": MINT, "use_skill": MINT,
}


def kind_color(name: str) -> str:
    if name.startswith("subagent_") or name == "parallel_tasks":
        return ACCENT
    return KIND_COLOR.get(name, "#a39c8c")


AGENT_NAMES = {"investigator": "Scout", "codebase_investigator": "Scout", "code_reviewer": "Reviewer", "planner": "Planner"}


def agent_title(agent: str) -> str:
    """Friendly name for a sub-agent: "investigator" reads as "Scout", "my_helper" as "My helper"."""
    return AGENT_NAMES.get(agent) or agent.replace("_", " ").capitalize()


def tool_parts(name: str, args: dict[str, Any], cwd: Path | None) -> tuple[str, str]:
    """(title, argument) for a call, e.g. ("Read", "src/a.py")."""
    if name.startswith("subagent_"):
        goal = str(args.get("goal", ""))
        return f"{agent_title(name[len('subagent_'):])} agent", goal[:70] + ("…" if len(goal) > 70 else "")
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
    return TITLES.get(name, name), val + extra


def tool_label(name: str, args: dict[str, Any], cwd: Path | None) -> str:
    title, arg = tool_parts(name, args, cwd)
    return f"{title}({arg})"


def header_text(name: str, args: dict[str, Any], cwd: Path | None) -> Text:
    """`▍ read  src/app.py`: a coloured chip for the tool, then its argument with the
    file name picked out of the path so it is the first thing the eye lands on."""
    title, arg = tool_parts(name, args, cwd)
    color = kind_color(name)
    out = Text.assemble((" ", ""), (f" {title.lower()} ", f"bold {INK} on {color}"), "  ")
    if PRIMARY_ARG.get(name) == "path" and "/" in arg:
        head, _, tail = arg.rpartition("/")
        out.append(head + "/", style=MUTED)
        out.append(tail, style=f"bold {FG}")
    elif name == "shell":
        out.append("$ ", style=MUTED)
        out.append(arg, style=f"bold {LAVENDER}")
    else:
        out.append(arg, style=f"bold {FG}")
    return out


class _Heading(Heading):
    """Left-aligned heading with a small bar, instead of rich's centred box."""

    def __rich_console__(self, console: Console, options: ConsoleOptions) -> RenderResult:
        self.text.justify = "left"
        yield Text.assemble(("▌ ", console.get_style(f"markdown.{self.tag}")), self.text)


class MewMarkdown(Markdown):
    elements = {**Markdown.elements, "heading_open": _Heading}


class Rail:
    """Draws a coloured bar down the left of whatever it wraps, however many lines it wraps to."""

    def __init__(self, body: Any, color: str, glyph: str = "│", first: str | None = None) -> None:
        self.body, self.color, self.glyph, self.first = body, color, glyph, first

    def __rich_console__(self, console: Console, options: ConsoleOptions) -> RenderResult:
        lines = console.render_lines(self.body, options.update_width(max(10, options.max_width - 3)), pad=False)
        for i, line in enumerate(lines):
            yield Segment(" " + (self.first if i == 0 and self.first else self.glyph) + " ", console.get_style(self.color))
            yield from line
            yield Segment.line()


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


ADD_BG, DEL_BG = "#1b2616", "#33181a"


def _highlight(code: str, lexer: str) -> Text:
    try:
        t = Syntax(code, lexer, theme="dracula").highlight(code)
        t.rstrip()
        # keep the token colours but drop the theme's own background, the diff row supplies it
        t.spans = [Span(sp.start, sp.end, Style(color=sp.style.color, bold=sp.style.bold, italic=sp.style.italic))
                   if isinstance(sp.style, Style) else sp for sp in t.spans]
        t.style = ""
        return t
    except Exception:
        return Text(code)


def render_diff(diff: str, max_lines: int = 16, path: str = "", width: int = 96) -> tuple[Text, int, int]:
    """Rows with line numbers, a +/- gutter, syntax-highlighted code and a tinted background
    that runs the full width, so added and removed lines read as blocks, not as coloured text."""
    rows, adds, dels = parse_unified_diff(diff)
    lexer = Syntax.guess_lexer(path, "") if path else "text"
    out = Text()
    shown = shown_changed = 0
    for kind, no, txt in rows:
        if shown >= max_lines:
            break
        bg = {"+": ADD_BG, "-": DEL_BG}.get(kind, "")
        row = Text(style=f"on {bg}" if bg else "")
        row.append(f"{no if no is not None else '':>4} ", style=MUTED)
        row.append(f"{kind} ", style=f"bold {MINT}" if kind == "+" else f"bold {CORAL}" if kind == "-" else MUTED)
        code = _highlight(txt[:160], lexer)
        if kind == " ":
            code.stylize("dim")
        row.append_text(code)
        row.truncate(width, overflow="ellipsis", pad=bool(bg))
        if out:
            out.append("\n")
        out.append_text(row)
        shown += 1
        shown_changed += kind in "+-"
    hidden = adds + dels - shown_changed
    if hidden > 0:
        out.append(f"\n   ⋯ +{hidden} more changed lines", style=MUTED)
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


def agent_stats(a: dict[str, Any], seconds: float) -> str:
    """`12s · 7 tools · 18k tok`"""
    tok = a.get("tokens", 0)
    parts = [f"{int(seconds)}s", f"{a['tools']} tool{'s' if a['tools'] != 1 else ''}"]
    if tok:
        parts.append(f"{tok / 1000:.0f}k tok" if tok >= 1000 else f"{tok} tok")
    return " · ".join(parts)


SPIN = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


def _mix(a: str, b: str, t: float) -> str:
    """Blend two #rrggbb colours: t=0 gives a, t=1 gives b."""
    x, y = (tuple(int(c[i:i + 2], 16) for i in (1, 3, 5)) for c in (a, b))
    return "#" + "".join(f"{round(p + (q - p) * t):02x}" for p, q in zip(x, y))


def shimmer(text: str, now: float) -> Text:
    """Pink-to-lavender wave that slides along the text, so a long wait still looks alive."""
    out = Text()
    for i, ch in enumerate(text):
        out.append(ch, style=_mix(ACCENT, LAVENDER, (math.sin(now * 4 - i * 0.45) + 1) / 2))
    return out
TIPS = [
    "@path attaches a file · Tab completes",
    "!git status runs a command and shows it to the model",
    "/compact frees context when a long session slows down",
    "/cost shows tokens, cache hits and why a cache missed",
    "Shift+Enter (or Ctrl+J) inserts a newline",
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
        self.quiet = True  # one line per tool call, no routine notices, one-line turn summary; /verbose turns it off
        # live turn state
        self.live: Live | None = None
        self._turn_active = False
        self._turn_started = 0.0
        self._buf = ""
        self._first_block = True
        self._out_chars = 0
        self._phase = "Thinking"
        self.todos: list[tuple[str, bool]] = []
        self.agents: dict[str, dict[str, Any]] = {}  # sub-agents running now
        self.agent_log: list[dict[str, Any]] = []  # ones that finished this turn
        self.results: deque = deque(maxlen=30)  # full tool results for /expand
        self.last_reply = ""
        self.fullscreen = False  # a full-screen pane shows the status line, so no Live region
        # what the side panel lists: files read, files written, commands run
        self.read_files: dict[str, int] = {}
        self.written: dict[str, tuple[int, int, bool]] = {}
        self.commands: list[tuple[str, bool]] = []
        self.touched: dict[str, float] = {}  # "kind:name" -> when it last changed, so the panel can glow

    def reset(self) -> None:
        """/clear: forget everything shown for the old conversation."""
        self.todos, self.commands, self.last_reply = [], [], ""
        self.agents.clear(), self.agent_log.clear(), self.results.clear()
        self.read_files.clear(), self.written.clear(), self.touched.clear()
        self._buf, self._first_block = "", True

    # -- live region -----------------------------------------------------
    def _live_ok(self) -> bool:
        return not self.fullscreen and self.console.is_terminal and not getattr(self.console, "record", False)

    def _renderable(self):
        rows: list[Any] = []
        now = time.monotonic()
        if self._buf.strip():
            tail = self._buf if self._buf.count("\n") < 14 else "…\n" + "\n".join(self._buf.split("\n")[-12:])
            rows.append(self._assistant_grid(tail, self._first_block))
        elapsed = int(now - self._turn_started)
        frame = SPIN[int(now * 10) % len(SPIN)]
        tokens = f"  ·  ↓ ~{self._out_chars / 4000:.1f}k tokens" if self._out_chars > 400 else ""
        status = Text.assemble((f" {frame} ", f"bold {ACCENT}"), shimmer(f"{self._phase}…", now))
        status.append(f"   {elapsed}s{tokens}  ·  ctrl+c to stop", style=MUTED)
        rows.append(status)
        rows.extend(self._agent_rows(now))
        if self.todos:
            done_n = sum(d for _, d in self.todos)
            rows.append(Text.assemble(("   ✦ tasks ", f"bold {LAVENDER}"), (f"{done_n}/{len(self.todos)}", MUTED)))
            for content, done in self.todos[:8]:
                rows.append(Text(f"     {'◼' if done else '◻'} {content[:90]}", style=f"{MUTED} strike" if done else FG))
        return Group(*rows)

    def _agent_rows(self, now: float) -> list[Text]:
        """Each sub-agent as a small block: goal and stats on top, its last few tool calls under it,
        the newest one marked as running. Finished ones stay as one line until the turn ends."""
        rows: list[Text] = []
        for a in self.agent_log:
            mark, color = ("✓", MINT) if a["ok"] else ("✗", CORAL)
            rows.append(Text.assemble((f"   {mark} ", color), (agent_title(a["agent"]), f"bold {FG}"), (f"  {a['goal']}", MUTED), (f"   {agent_stats(a, a['seconds'])}", FAINT)))
        for a in self.agents.values():
            rows.append(Text.assemble(("   ◆ ", ACCENT), (agent_title(a["agent"]), f"bold {ACCENT}"), (f"  {a['goal']}", FG), (f"   {agent_stats(a, now - a['started'])}", MUTED)))
            recent = list(a["recent"])
            for i, label in enumerate(recent):
                last = i == len(recent) - 1
                rows.append(Text.assemble(("   ╰ " if last else "   │ ", RULE), ("▸ " if last else "✓ ", ACCENT if last else FAINT), (label, FG if last else MUTED)))
            if not recent:
                rows.append(Text.assemble(("   ╰ ", RULE), ("starting…", MUTED)))
        return rows

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
        self.agent_log.clear()

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

        cat = Text.assemble((" /\\_/\\ \n", ACCENT), ("( ", ACCENT), ("o.o", MINT), (" )\n", ACCENT), (" > ^ < ", ACCENT))
        info = Text.assemble((title, f"bold {ACCENT}"), ("  ·  a small agent for real repos\n", MUTED))
        if not self.fullscreen:  # the full-screen header and footer already show model, directory and mode
            for ln in lines:
                # "model: x   approval: y" becomes lavender keys over soft text
                for part in re.split(r"(\w+:)", ln):
                    info.append(part, style=LAVENDER if part.endswith(":") and " " not in part else MUTED)
                info.append("\n")
        else:
            info.append("/help commands  ·  @file attach  ·  !cmd run  ·  drag to copy", style=MUTED)
        info.rstrip()
        grid = Table.grid(padding=(0, 3))
        grid.add_row(cat, info)
        self.console.print()
        self.console.print(Padding(grid, (0, 2)))
        if not self.quiet:
            self.console.print(Text(f"  ✦ tip  {random.choice(TIPS)}", style=f"italic {MUTED}"))
        self.console.print()

    def echo_user(self, text: str) -> None:
        first, *rest = text.strip().split("\n")
        shown = first + (f"  (+{len(rest)} lines)" if rest else "")
        self.console.print(Padding(Text.assemble(("▍ ", f"bold {ACCENT}"), (shown[:300], f"bold {FG}")), (0, 1), style=f"on {BG_BOX}"))

    def _assistant_grid(self, md: str, first: bool):
        return Rail(MewMarkdown(md.rstrip(), code_theme="dracula"), LAVENDER, "▎", "◆" if first else None)

    def _print_block(self, md: str) -> None:
        if not md.strip():
            return
        self.console.print()
        self.console.print(self._assistant_grid(md, self._first_block))
        self.last_reply = md.strip() if self._first_block else self.last_reply + "\n\n" + md.strip()  # for /copy
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

    ROUTINE_NOTICES = ("Loaded playbooks", "Scout finished", "Routing to", "Working in", "Challenger is questioning",
                       "steering received", "Context compacted", "Cache had expired")

    def notice(self, message: str) -> None:
        if self.quiet and message.startswith(self.ROUTINE_NOTICES):
            return  # things the harness did that you do not need to act on
        self.console.print(Text(f"  ✦ {message}", style=f"italic {MUTED}"))

    def error(self, message: str) -> None:
        self.assistant_flush()
        self.console.print()
        self.console.print(Rail(Text(message, style=CORAL), CORAL, "┃", "✗"))

    def set_todos(self, todos: list[tuple[str, bool]]) -> None:
        self.todos = todos

    def agent_progress(self, call_id: str, info: dict[str, Any]) -> None:
        kind = info.get("kind")
        if kind == "start":
            goal = " ".join(str(info.get("goal", "")).split())
            self.agents[call_id] = {"agent": info["agent"], "goal": goal[:60] + ("…" if len(goal) > 60 else ""), "started": time.monotonic(),
                                    "recent": deque(maxlen=3), "tools": 0, "tokens": 0}
        elif kind == "tool" and call_id in self.agents:
            name, args = info.get("name", ""), info.get("args", {})
            a = self.agents[call_id]
            a["recent"].append(tool_label(name, args, self.cwd))
            a["tools"] += 1
            a["tokens"] = info.get("tokens", a["tokens"])
            if name == "read_file" and args.get("path"):  # files a sub-agent opens count as read too
                path = display_path_rel_to_cwd(str(args["path"]), self.cwd)
                self.read_files[path] = self.read_files.get(path, 0) + 1
                self.touched["read:" + path] = time.monotonic()
        elif kind == "done":
            a = self.agents.pop(call_id, None)
            if a:
                a["tokens"] = info.get("tokens", a["tokens"])
                self.agent_log.append({**a, "ok": info.get("ok", True), "seconds": time.monotonic() - a["started"]})

    def tool_call_start(self, call_id: str, name: str, tool_kind: str | None, arguments: dict[str, Any]) -> None:
        """Only remembers the call. The header is printed together with its result
        (or with the approval prompt), so parallel calls never interleave."""
        self.assistant_flush()
        self._calls[call_id] = (name, arguments)
        self._phase = f"Running {tool_label(name, arguments, self.cwd)}" if len(self._calls) == 1 else f"Running {len(self._calls)} tools"

    def _print_header(self, name: str, args: dict[str, Any]) -> None:
        self.console.print()
        self.console.print(header_text(name, args, self.cwd))

    def tool_call_complete(self, call_id: str, name: str, tool_kind: str | None, success: bool, output: str,
                           error: str | None, metadata: dict[str, Any] | None, diff: str | None,
                           truncated: bool, exit_code: int | None) -> None:
        args = self._calls.pop(call_id, (name, {}))[1]
        self._phase = "Thinking"
        md = metadata or {}
        self._track(name, args, success, md, diff)
        self.results.append({"label": tool_label(name, args, self.cwd), "success": success, "output": output, "error": error, "diff": diff})
        summary, preview = summarize_result(name, success, output, error, md, exit_code, args)
        is_edit = name in {"edit", "write_file"} and success and diff
        if self.quiet and not is_edit:
            # `read  src/app.py  ·  Read 40 lines` on a single line; /expand shows the full output
            line = header_text(name, args, self.cwd)
            line.append("  ·  " + summary.removeprefix("✗ "), style=CORAL if not success else MUTED)
            if self._header_done:  # an approval prompt already showed the header
                line = Text("  " + ("✗ " if not success else "✓ ") + summary.removeprefix("✗ "), style=CORAL if not success else MUTED)
                self._header_done = False
            self.console.print(line)
            return
        if self._header_done:
            self._header_done = False
        else:
            self._print_header(name, args)
        color = kind_color(name) if success else CORAL
        rows: list[Any] = []
        if name in {"edit", "write_file"} and success and diff:
            path = display_path_rel_to_cwd(md.get("path", args.get("path", "")), self.cwd)
            body, adds, dels = render_diff(diff, 400 if self.expand else (8 if self.quiet else 14), path, min(self.console.width, 120) - 6)
            what = Text.assemble(*(x for x in ((f"+{adds}", f"bold {MINT}"), (" ", ""), (f"−{dels}", f"bold {CORAL}")) if x[0] not in ("+0", "−0")))
            verb = "created" if md.get("is_new_file") else "updated"
            rows.append(Text.assemble(("✓ ", f"bold {MINT}"), (f"{verb}  ", MUTED), what))
            rows.append(body)
        else:
            mark = ("✗ ", f"bold {CORAL}") if not success else ("✓ ", f"bold {MINT}")
            rows.append(Text.assemble(mark, (summary.removeprefix("✗ "), CORAL if not success else FG)))
            for ln in preview[: 60 if self.expand else None]:
                rows.append(Text(f"  {ln[:200]}", style=MUTED))
        self.console.print(Rail(Group(*rows), color, "│"))

    def _track(self, name: str, args: dict[str, Any], success: bool, md: dict, diff: str | None) -> None:
        if not success:
            cmd = str(args.get("command", "")).strip()
            if name == "shell" and cmd and cmd.split()[0] not in QUIET_COMMANDS:
                self.commands.append((cmd, False))
                self.touched[f"cmd:{len(self.commands) - 1}"] = time.monotonic()
            return
        path = display_path_rel_to_cwd(md.get("path", args.get("path", "")), self.cwd) if (md.get("path") or args.get("path")) else ""
        if name == "read_file" and path:
            self.read_files[path] = self.read_files.get(path, 0) + 1
            self.touched["read:" + path] = time.monotonic()
        elif name in {"edit", "write_file"} and path:
            _, adds, dels = parse_unified_diff(diff) if diff else ([], 0, 0)
            old = self.written.get(path, (0, 0, False))
            self.written[path] = (old[0] + adds, old[1] + dels, old[2] or bool(md.get("is_new_file")))
            self.touched["write:" + path] = time.monotonic()
        elif name == "shell":
            cmd = str(args.get("command", "")).strip()
            if cmd and cmd.split()[0] not in QUIET_COMMANDS:
                self.commands.append((cmd, True))
                self.touched[f"cmd:{len(self.commands) - 1}"] = time.monotonic()

    def show_result(self, n: int = 1) -> None:
        """/expand [n]: print the full output of the n-th most recent tool call."""
        if not self.results or n < 1 or n > len(self.results):
            self.console.print("[dim]  no such tool result[/dim]")
            return
        r = list(self.results)[-n]
        if r["diff"]:
            body: Any = render_diff(r["diff"], 1000, width=min(self.console.width, 120) - 6)[0]
        else:
            text = (r["error"] or "") + ("\n" if r["error"] and r["output"] else "") + (r["output"] or "")
            lines = text.splitlines()
            body = Syntax("\n".join(lines[:300]) + (f"\n… +{len(lines) - 300} lines" if len(lines) > 300 else ""), "text", word_wrap=True, theme="monokai")
        self.console.print(Panel(body, title=Text(f" {r['label']} ", style=f"bold {FG}"), title_align="left", border_style="border", box=box.ROUNDED))

    def turn_summary(self, elapsed: float, usage: dict | None, report: dict | None) -> None:
        out = Text.assemble(("  ✦ ", ACCENT), (f"{elapsed:.0f}s", f"bold {FG}"))
        if self.quiet and usage and usage.get("calls"):
            out.append(f"  ·  {usage['calls']} call{'s' if usage['calls'] != 1 else ''}  ·  {usage['prompt_tokens'] / 1000:.0f}k in, {usage['cache_hit_pct']:.0f}% cached", style=MUTED)
            if usage.get("est_cost_usd") is not None:
                out.append(f"  ·  ${usage['est_cost_usd']}", style=AMBER)
        elif usage and usage.get("calls"):
            out.append(f"  ·  {usage['calls']} call{'s' if usage['calls'] != 1 else ''}  ·  {usage['prompt_tokens'] / 1000:.1f}k in ", style=MUTED)
            out.append(f"({usage['cache_hit_pct']:.0f}% cached, {(usage['prompt_tokens'] - usage.get('cached_tokens', 0)) / 1000:.1f}k new)", style=MINT if usage["cache_hit_pct"] >= 50 else AMBER)
            out.append(f"  ·  {usage['completion_tokens'] / 1000:.1f}k out", style=MUTED)
            if usage.get("est_cost_usd") is not None:
                out.append(f"  ·  ${usage['est_cost_usd']}", style=AMBER)
        self.console.print(out)

    # -- approval ------------------------------------------------------
    async def confirm(self, confirmation: ToolConfirmation) -> Decision:
        from ui.prompt import ask_choice, read_line

        self.pause()
        try:
            if confirmation.command:
                parts: list[Any] = [Syntax(confirmation.command, "bash", theme="dracula", background_color="default", word_wrap=True)]
                title, q = "Run command", "Do you want to run this command?"
                always = "Yes, and don't ask again for this kind of command this session"
            elif confirmation.diff:
                parts = [render_diff(confirmation.diff.to_diff(), 40, confirmation.diff.path, min(self.console.width, 120) - 8)[0]]
                title, q = confirmation.description, "Do you want to make this edit?"
                always = "Yes, and don't ask again for edits this session"
            else:
                parts = [Text(confirmation.description)]
                title, q, always = confirmation.tool_name, "Do you want to proceed?", "Yes, and don't ask again this session"
            self._print_header(confirmation.tool_name, confirmation.params)
            self._header_done = True
            danger = AMBER if confirmation.command else LAVENDER
            self.console.print(Panel(Group(*parts), title=Text(f" {title.split(chr(10))[0]} ", style=f"bold {danger}"), title_align="left",
                                     border_style=danger, box=box.ROUNDED, padding=(0, 1)))
            self._phase = "Waiting for your answer"
            self.console.bell()  # a run that waits unnoticed for minutes looks stuck
            choice = await ask_choice(self.console, q, ["Yes", always, "No, and tell me what to do differently"])
            if choice < 2:  # answer at once, the command itself may take a while
                self.console.print(Text("  ▸ approved, running…", style=MINT))
                self._phase = f"Running {tool_label(confirmation.tool_name, confirmation.params, self.cwd)}"
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
        self.console.print(MewMarkdown(
            "## Commands\n" + rows
            + "\n\n## Input\n- `@path` attaches a file (Tab completes)\n"
            "- `!cmd` runs a shell command and shows the output to the model; `!!cmd` runs it privately\n"
            "- `Shift+Enter` or `Ctrl+J` inserts a newline · `Ctrl+C` interrupts the agent\n"
        ))
