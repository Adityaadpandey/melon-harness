"""Input line (history, completion, status footer) and small interactive helpers."""
from __future__ import annotations

import asyncio
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

ACCENT = "#ff7eb6"
LAVENDER = "#b49cff"
MUTED = "#6c7393"
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
    "/copy": "copy the last reply to the clipboard",
    "/jobs": "background commands: /jobs, /jobs 1 for output, /jobs kill 1",
    "/expand": "show the full output of the last tool call (/expand 2 for the one before)",
    "/save": "save this session",
    "/sessions": "list saved sessions",
    "/resume": "resume a saved session (give its ID)",
    "/checkpoint": "create a checkpoint",
    "/undo": "put back the files the last request changed",
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
                 toolbar: Callable[[], str], history_file: Path | None = None, input=None, output=None,
                 body=None, extra_kb: KeyBindings | None = None, mouse=None):
        from prompt_toolkit.application import Application
        from prompt_toolkit.filters import Condition
        from prompt_toolkit.key_binding import merge_key_bindings
        from prompt_toolkit.application import get_app
        from prompt_toolkit.layout import Float, FloatContainer, HSplit, Layout, VSplit, Window
        from prompt_toolkit.layout.controls import FormattedTextControl
        from prompt_toolkit.layout.dimension import Dimension
        from prompt_toolkit.layout.menus import CompletionsMenu
        from prompt_toolkit.widgets import TextArea
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
        self.fullscreen = body is not None
        self.queue: asyncio.Queue = asyncio.Queue()  # full-screen mode: submitted lines wait here
        self.choice: tuple[int, asyncio.Future] | None = None
        self.question: tuple[str, list[str]] | None = None  # shown by the full-screen status area
        self.line_future: asyncio.Future | None = None
        self.on_interrupt: Callable[[], None] | None = None
        self.is_busy: Callable[[], bool] = lambda: False
        self._task: asyncio.Task | None = None
        self.area = TextArea(
            prompt=[("class:prompt", " ✦ ")], style="class:box", dont_extend_height=True, multiline=True, wrap_lines=True, history=history,
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
            if self.fullscreen:
                text = buf.text
                if self.line_future:  # an answer to a question the agent asked
                    fut, self.line_future = self.line_future, None
                    buf.reset()
                    fut.set_result(text.strip())
                elif text.strip():
                    buf.append_to_history()
                    buf.reset()
                    if self.is_busy():  # say so, otherwise Enter looks like it did nothing
                        from ui.tui import get_console

                        get_console().print(f"  [dim]⏳ queued, runs after this turn:[/dim] {text.strip().splitlines()[0][:80]}")
                    self.queue.put_nowait(text)
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
            if self.fullscreen:
                if self.choice:
                    self._pick(self.choice[0] - 1)
                elif buf.text:
                    buf.reset()
                elif self.on_interrupt:
                    self.on_interrupt()
                else:
                    self.queue.put_nowait(KeyboardInterrupt())
                return
            if buf.text:
                buf.reset()  # first Ctrl+C clears the line, like a shell
            else:
                event.app.exit(exception=KeyboardInterrupt)

        @kb.add("c-d")
        def _ctrl_d(event):
            if self.fullscreen and not buf.text:
                self.queue.put_nowait(EOFError())
            elif not buf.text:
                event.app.exit(exception=EOFError)

        def footer_text():
            # approval mode as a chip, model in pink, context bar tinted by how full it is
            frags: list[tuple[str, str]] = [("", " ")]
            for i, part in enumerate(self._toolbar().split("  ·  ")):
                if i:
                    frags.append(("class:tb.dot", "  ·  "))
                if i == 0:
                    frags.append(("class:tb.chip", f" {part} "))
                elif i == 1:
                    frags.append(("class:tb.model", part))
                elif part.startswith("ctx"):
                    pct = int(re.search(r"(\d+)%", part).group(1)) if re.search(r"(\d+)%", part) else 0
                    frags.append(("class:tb.ok" if pct < 60 else "class:tb.warn" if pct < 85 else "class:tb.bad", part))
                elif part.startswith("$"):
                    frags.append(("class:tb.warn", part))
                else:
                    frags.append(("class:toolbar", part))
            return frags

        # single-key answers while an approval question is open (they win over typing)
        asking = Condition(lambda: self.choice is not None)
        for i in range(9):
            kb.add(str(i + 1), filter=asking & Condition(lambda i=i: self.choice is not None and i < self.choice[0]))(lambda e, i=i: self._pick(i))
        kb.add("y", filter=asking)(lambda e: self._pick(0))
        kb.add("enter", filter=asking)(lambda e: self._pick(0))
        for key in ("n", "escape"):
            kb.add(key, filter=asking)(lambda e: self._pick(self.choice[0] - 1))
        kb.add("<any>", filter=asking)(lambda e: None)  # swallow stray typing while a question is open

        hints = [("class:tb.key", " enter "), ("class:toolbar", " send  "), ("class:tb.key", " alt+enter "), ("class:toolbar", " newline  "),
                 ("class:tb.key", " / "), ("class:toolbar", " commands  "), ("class:tb.key", " @ "), ("class:toolbar", " files ")]

        def footer_all():
            left = footer_text()
            used = sum(len(t) for _, t in left)
            room = get_app().output.get_size().columns - used - sum(len(t) for _, t in hints)
            return left + ([("class:toolbar", " " * room)] + hints if self.fullscreen and room > 2 else [])

        footer = Window(FormattedTextControl(footer_all), height=1, style="class:statusbar")
        bar = Window(width=1, char="▍", style=f"fg:{ACCENT} bg:#1d2033")  # accent edge down the whole input box
        input_box = [Window(height=1, style="class:box"), VSplit([bar, self.area]), Window(height=1, style="class:box"), footer]
        root = FloatContainer(
            content=HSplit(([body] if body is not None else []) + input_box),
            floats=[Float(xcursor=True, ycursor=True, content=CompletionsMenu(max_height=8, scroll_offset=1))],
        )
        self.app = Application(
            layout=Layout(root, focused_element=self.area), key_bindings=merge_key_bindings([kb, extra_kb] if extra_kb else [kb]),
            erase_when_done=not self.fullscreen, full_screen=self.fullscreen, mouse_support=(mouse if mouse is not None else self.fullscreen),
            refresh_interval=0.1 if self.fullscreen else None,
            style=Style.from_dict({
                "prompt": f"bg:#1d2033 {ACCENT} bold", "box": "bg:#1d2033 #d5d9ec", "statusbar": "bg:#151826", "tb.key": "bg:#2a2e48 #c5cae9", "bar": f"bg:#1d2033 {ACCENT} bold", "toolbar": MUTED, "tb.dot": "#3a3f5c",
                "tb.chip": f"bg:{LAVENDER} #14161f bold", "tb.model": f"{ACCENT} bold", "tb.ok": "#6ee7b7", "tb.warn": "#fbbf6a", "tb.bad": "#ff6b7f",
                "completion-menu.completion": "bg:#1d2033 #c5cae9", "completion-menu.completion.current": f"bg:{ACCENT} #14161f bold",
                "completion-menu.meta.completion": "bg:#1d2033 #6c7393", "completion-menu.meta.completion.current": f"bg:{ACCENT} #14161f",
            }),
            input=input, output=output,
        )

    def _pick(self, i: int) -> None:
        if self.choice:
            fut, self.choice = self.choice[1], None
            fut.set_result(i)

    async def choose(self, n: int) -> int:
        """Full-screen: wait for a single keypress picking one of n options."""
        fut = asyncio.get_running_loop().create_future()
        self.choice = (n, fut)
        self.app.invalidate()
        return await fut

    async def line(self) -> str:
        """Full-screen: the next line typed, as the answer to a question."""
        self.line_future = asyncio.get_running_loop().create_future()
        return await self.line_future

    async def read(self) -> str:
        if not self.fullscreen:
            self.area.buffer.reset()
            return await self.app.run_async()
        if self._task is None:
            self._task = asyncio.create_task(self.app.run_async())
        getter = asyncio.ensure_future(self.queue.get())
        done, _ = await asyncio.wait({getter, self._task}, return_when=asyncio.FIRST_COMPLETED)
        if getter not in done:
            getter.cancel()
            raise EOFError
        item = getter.result()
        if isinstance(item, BaseException):
            raise item
        return item

    async def close(self) -> None:
        if self._task is not None:
            if self.app.is_running:
                self.app.exit()
            await asyncio.gather(self._task, return_exceptions=True)


ACTIVE: InputSession | None = None  # the full-screen input, when there is one


async def ask_choice(console, question: str, options: list[str], *, input=None, output=None) -> int:
    """Numbered choice answered with a single keypress. Esc / Ctrl+C pick the last option (No)."""
    if ACTIVE is not None:  # full-screen: the options live above the input and vanish once answered
        ACTIVE.question = (question, options)
        try:
            return await ACTIVE.choose(len(options))
        finally:
            ACTIVE.question = None
    console.print(f"\n [bold {LAVENDER}]?[/] [bold]{question}[/bold]")
    for i, opt in enumerate(options, 1):
        key = "#ff6b7f" if i == len(options) else ACCENT
        console.print(f"  [bold {key}]\\[{i}][/] {opt}" + ("  [dim](enter)[/dim]" if i == 1 else ""))
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
    if ACTIVE is not None:
        from ui.tui import get_console

        get_console().print(f"  [dim]{prompt.strip()}[/dim]")
        return await ACTIVE.line()
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
