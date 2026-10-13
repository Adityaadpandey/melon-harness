"""Input line (history, completion, status footer) and small interactive helpers."""
from __future__ import annotations

import re
import time
from pathlib import Path
from typing import Callable

from prompt_toolkit import PromptSession
from prompt_toolkit.completion import Completer, Completion, ThreadedCompleter
from prompt_toolkit.history import FileHistory, InMemoryHistory
from prompt_toolkit.key_binding import KeyBindings
from prompt_toolkit.styles import Style

from utils.search import list_files

ACCENT = "#d97757"
MAX_ATTACH_BYTES = 60_000

COMMANDS = {
    "/help": "show commands and shortcuts",
    "/clear": "clear the conversation",
    "/compact": "summarize older messages now to free context",
    "/cost": "tokens, cache hit rate, cache misses, estimated cost",
    "/model": "show or switch model (Tab completes ids)",
    "/approval": "set approval mode: on-request, auto-edit, auto, never, yolo",
    "/config": "show current configuration",
    "/stats": "session statistics",
    "/tools": "list available tools",
    "/skills": "list project playbooks",
    "/mcp": "MCP server status",
    "/expand": "show the full output of the last tool call (/expand 2 for the one before)",
    "/save": "save this session",
    "/sessions": "list saved sessions",
    "/resume": "resume a saved session (give its ID)",
    "/checkpoint": "create a checkpoint",
    "/restore": "restore a checkpoint (give its ID)",
    "/exit": "quit",
}


class MewCompleter(Completer):
    def __init__(self, cwd: Path, forbidden: list[str], models: Callable[[], list[str]]):
        self.cwd, self.forbidden, self.models = cwd, forbidden, models
        self._files: list[str] = []
        self._files_at = 0.0

    def _file_list(self) -> list[str]:
        if time.monotonic() - self._files_at > 20:
            self._files = [p.relative_to(self.cwd).as_posix() for p in list_files(self.cwd, self.forbidden)]
            self._files_at = time.monotonic()
        return self._files

    def get_completions(self, document, complete_event):
        text = document.text_before_cursor
        if text.startswith("/") and " " not in text:
            for cmd, desc in COMMANDS.items():
                if cmd.startswith(text):
                    yield Completion(cmd, start_position=-len(text), display_meta=desc)
            return
        if text.startswith("/model "):
            arg = text[len("/model "):]
            for m in self.models():
                if arg.lower() in m.lower():
                    yield Completion(m, start_position=-len(arg))
            return
        m = re.search(r"@([^\s@]*)$", text)
        if m:
            q = m.group(1).lower()
            hits = [f for f in self._file_list() if q in f.lower()]
            hits.sort(key=lambda f: (not f.lower().startswith(q), q not in Path(f).name.lower(), len(f)))
            for f in hits[:40]:
                yield Completion("@" + f, start_position=-len(m.group(0)))


class InputSession:
    """A framed input box: grows with the text, completes / commands and @files,
    walks history with Up/Down, Alt+Enter for a newline, with a status footer."""

    def __init__(self, cwd: Path, forbidden: list[str], models: Callable[[], list[str]],
                 toolbar: Callable[[], str], history_file: Path | None = None, input=None, output=None):
        from prompt_toolkit.application import Application
        from prompt_toolkit.filters import Condition
        from prompt_toolkit.layout import Float, FloatContainer, HSplit, Layout, Window
        from prompt_toolkit.layout.controls import FormattedTextControl
        from prompt_toolkit.layout.dimension import Dimension
        from prompt_toolkit.layout.menus import CompletionsMenu
        from prompt_toolkit.widgets import Frame, TextArea
        from prompt_toolkit.widgets.base import Border

        # rounded corners, like the rest of the UI
        Border.TOP_LEFT, Border.TOP_RIGHT, Border.BOTTOM_LEFT, Border.BOTTOM_RIGHT = "╭", "╮", "╰", "╯"

        try:
            if history_file:
                history_file.parent.mkdir(parents=True, exist_ok=True)
            history = FileHistory(str(history_file)) if history_file else InMemoryHistory()
        except OSError:
            history = InMemoryHistory()

        self._toolbar = toolbar
        self.area = TextArea(
            prompt=[("class:prompt", "❯ ")], multiline=True, wrap_lines=True, history=history,
            completer=ThreadedCompleter(MewCompleter(cwd, forbidden, models)), complete_while_typing=True,
            height=lambda: Dimension(min=1, max=12, preferred=max(1, self.area.document.line_count)),
        )
        kb = KeyBindings()
        buf = self.area.buffer

        @kb.add("enter")
        def _enter(event):
            cs = buf.complete_state
            if cs and cs.current_completion:  # a highlighted suggestion: take it instead of submitting
                buf.apply_completion(cs.current_completion)
                return
            if buf.text.strip():
                buf.append_to_history()
            event.app.exit(result=buf.text)

        @kb.add("escape", "enter")  # Alt+Enter
        @kb.add("c-j")
        def _newline(event):
            buf.insert_text("\n")

        @kb.add("tab")
        def _tab(event):
            if buf.complete_state:
                buf.complete_next()
            else:
                buf.start_completion(select_first=True)

        @kb.add("s-tab")
        def _stab(event):
            if buf.complete_state:
                buf.complete_previous()

        @kb.add("up")
        def _up(event):
            if buf.complete_state:
                buf.complete_previous()
            elif buf.document.cursor_position_row == 0:
                buf.history_backward()
            else:
                buf.cursor_up()

        @kb.add("down")
        def _down(event):
            if buf.complete_state:
                buf.complete_next()
            elif buf.document.cursor_position_row >= buf.document.line_count - 1:
                buf.history_forward()
            else:
                buf.cursor_down()

        @kb.add("c-c")
        def _ctrl_c(event):
            if buf.text:
                buf.reset()  # first Ctrl+C clears the line, like a shell
            else:
                event.app.exit(exception=KeyboardInterrupt)

        @kb.add("c-d")
        def _ctrl_d(event):
            if not buf.text:
                event.app.exit(exception=EOFError)

        footer = Window(FormattedTextControl(lambda: [("class:toolbar", " " + self._toolbar())]), height=1)
        root = FloatContainer(
            content=HSplit([Frame(self.area, style="class:frame"), footer]),
            floats=[Float(xcursor=True, ycursor=True, content=CompletionsMenu(max_height=8, scroll_offset=1))],
        )
        self.app = Application(
            layout=Layout(root, focused_element=self.area), key_bindings=kb, erase_when_done=True,
            style=Style.from_dict({
                "prompt": f"{ACCENT} bold", "frame.border": "#5f5f5f", "toolbar": "#7f7f7f",
                "completion-menu.completion": "bg:#262626 #bbbbbb", "completion-menu.completion.current": f"bg:{ACCENT} #000000",
            }),
            input=input, output=output,
        )

    async def read(self) -> str:
        self.area.buffer.reset()
        return await self.app.run_async()


async def ask_choice(console, question: str, options: list[str], *, input=None, output=None) -> int:
    """Numbered choice answered with a single keypress. Esc / Ctrl+C pick the last option (No)."""
    console.print(f"[bold]{question}[/bold]")
    for i, opt in enumerate(options, 1):
        console.print(f"  [accent]{'❯' if i == 1 else ' '}[/accent] {i}. {opt}")
    kb = KeyBindings()
    for i in range(len(options)):
        kb.add(str(i + 1))(lambda e, i=i: e.app.exit(result=i))
    kb.add("y")(lambda e: e.app.exit(result=0))
    kb.add("enter")(lambda e: e.app.exit(result=0))
    for key in ("n", "escape", "c-c"):
        kb.add(key, eager=True)(lambda e: e.app.exit(result=len(options) - 1))
    try:
        return int(await PromptSession(key_bindings=kb, input=input, output=output).prompt_async([("class:prompt", f"  Choose [1-{len(options)}]: ")], style=Style.from_dict({"prompt": ACCENT})))
    except (EOFError, KeyboardInterrupt):
        return len(options) - 1


async def read_line(prompt: str) -> str:
    try:
        return (await PromptSession().prompt_async(prompt)).strip()
    except (EOFError, KeyboardInterrupt):
        return ""


def expand_mentions(text: str, cwd: Path, forbidden: list[str]) -> tuple[str, list[str]]:
    """Attach `@path` files to the message. Secrets and protected paths are never attached."""
    from safety.guard import is_forbidden_path
    from utils.search import _is_secret

    attached: list[str] = []
    blocks: list[str] = []
    for m in re.finditer(r"(?<!\S)@([\w./\-]+)", text):
        rel = m.group(1)
        f = (cwd / rel).resolve()
        if rel in attached or not f.exists() or _is_secret(f.name) or is_forbidden_path(rel, forbidden):
            continue
        if f.is_file() and f.stat().st_size <= MAX_ATTACH_BYTES:
            try:
                blocks.append(f'<file path="{rel}">\n{f.read_text(encoding="utf-8", errors="replace")}\n</file>')
            except OSError:
                continue
        elif f.is_dir():
            names = sorted(p.name + ("/" if p.is_dir() else "") for p in f.iterdir() if not _is_secret(p.name))[:100]
            blocks.append(f'<directory path="{rel}">\n' + "\n".join(names) + "\n</directory>")
        else:
            continue
        attached.append(rel)
    return (text + "\n\n" + "\n\n".join(blocks) if blocks else text), attached
