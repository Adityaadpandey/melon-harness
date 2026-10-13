"""Full-screen layout: the conversation on the left, a side panel on the right listing the files
read, the files written and the commands run, then the status line and the input box underneath.

Everything the agent prints still goes through the shared rich console. In full-screen mode that
console writes into a Sink instead of the terminal, and the transcript pane shows the tail of it."""
from __future__ import annotations

import io
import os
from typing import Any

from prompt_toolkit.application import get_app
from prompt_toolkit.filters import Condition
from prompt_toolkit.formatted_text import ANSI
from prompt_toolkit.key_binding import KeyBindings
from prompt_toolkit.layout import ConditionalContainer, HSplit, VSplit, Window
from prompt_toolkit.layout.controls import FormattedTextControl
from prompt_toolkit.layout.dimension import Dimension
from prompt_toolkit.mouse_events import MouseEventType
from rich.console import Console

from ui.tui import ACCENT, AGENT_THEME, AMBER, CORAL, FG, LAVENDER, MINT, MUTED, SKY, TUI, redirect_console

SIDE_WIDTH = 38
MIN_COLUMNS = 100  # narrower terminals get the conversation only
MAX_LINES = 6000


class Sink:
    """File-like target for the console: keeps what was printed and nudges a redraw."""

    def __init__(self) -> None:
        self.text = ""
        self.on_write = lambda: None

    def write(self, s: str) -> int:
        self.text += s
        if self.text.count("\n") > MAX_LINES * 1.2:  # keep memory bounded on very long sessions
            self.text = "\n".join(self.text.split("\n")[-MAX_LINES:])
        self.on_write()
        return len(s)

    def flush(self) -> None:
        pass

    def isatty(self) -> bool:
        return True


def _size() -> tuple[int, int]:
    try:
        o = get_app().output.get_size()
        return o.columns, o.rows
    except Exception:
        t = os.get_terminal_size()
        return t.columns, t.lines


def _clip(path: str, width: int) -> str:
    """Keep the end of a long path, where the file name is."""
    return path if len(path) <= width else "…" + path[-(width - 1):]


class _Pane(FormattedTextControl):
    """Transcript text that reacts to the mouse wheel."""

    def __init__(self, text, on_scroll) -> None:
        super().__init__(text)
        self._on_scroll = on_scroll

    def mouse_handler(self, mouse_event):
        if mouse_event.event_type == MouseEventType.SCROLL_UP:
            self._on_scroll(3)
        elif mouse_event.event_type == MouseEventType.SCROLL_DOWN:
            self._on_scroll(-3)
        else:
            return NotImplemented


class Screen:
    def __init__(self, tui: TUI, console: Console) -> None:
        self.tui, self.console = tui, console
        self.sink = Sink()
        self.offset = 0  # lines scrolled up from the bottom; 0 follows new output
        self.kb = KeyBindings()
        self._status_height = 0
        self._status_console = Console(file=io.StringIO(), force_terminal=True, color_system="truecolor", theme=AGENT_THEME, highlight=False)
        cols, _ = _size()
        redirect_console(console, self.sink, self._main_width(cols))
        tui.fullscreen = True

        @self.kb.add("pageup")
        def _pgup(event):
            self.scroll(self._main_rows() - 2)

        @self.kb.add("pagedown")
        def _pgdn(event):
            self.scroll(-(self._main_rows() - 2))

        self.main = Window(_Pane(self._transcript, self.scroll), wrap_lines=False)
        self.status = Window(FormattedTextControl(self._status), height=lambda: Dimension.exact(self._status_height))
        self.sections = [Window(FormattedTextControl(lambda k=k: self._section(k)), wrap_lines=False, style="bg:#151826") for k in ("read", "write", "cmd")]
        side = VSplit([
            Window(width=1, char="│", style="fg:#3a3f5c"),
            HSplit([self.sections[0], Window(height=1, char="─", style="fg:#3a3f5c bg:#151826"), self.sections[1],
                    Window(height=1, char="─", style="fg:#3a3f5c bg:#151826"), self.sections[2]], width=SIDE_WIDTH, style="bg:#151826"),
        ])
        wide = Condition(lambda: _size()[0] >= MIN_COLUMNS)
        self.body = HSplit([
            VSplit([self.main, ConditionalContainer(side, filter=wide)]),
            self.status,
        ])
        self.sink.on_write = self._poke

    # -- layout helpers ------------------------------------------------
    def _main_width(self, cols: int) -> int:
        return max(40, cols - (SIDE_WIDTH + 1 if cols >= MIN_COLUMNS else 0) - 1)

    def _main_rows(self) -> int:
        info = self.main.render_info
        return info.window_height if info else max(5, _size()[1] - 8)

    def _poke(self) -> None:
        try:
            get_app().invalidate()
        except Exception:
            pass

    def scroll(self, delta: int) -> None:
        total = self.sink.text.count("\n")
        self.offset = max(0, min(total, self.offset + delta))

    # -- panes ---------------------------------------------------------
    def _transcript(self):
        self.console.width = self._main_width(_size()[0])  # follows terminal resizes
        rows = self._main_rows()
        lines = self.sink.text.split("\n")
        if lines and lines[-1] == "":
            lines.pop()
        end = max(0, len(lines) - self.offset)
        shown = lines[max(0, end - rows):end]
        if self.offset:
            shown = shown[:-1] + [f"\x1b[38;2;251;191;106m  ↓ {self.offset} lines below · PageDown or scroll to return\x1b[0m"] if shown else shown
        return ANSI("\n".join(shown))

    def _status(self):
        if not self.tui._turn_active:
            self._status_height = 0
            return ""
        c = self._status_console
        c.width = self._main_width(_size()[0])
        c.file = io.StringIO()
        c.print(self.tui._renderable())
        out = c.file.getvalue().rstrip("\n")
        self._status_height = out.count("\n") + 1
        return ANSI(out)

    def _section(self, kind: str):
        t = self.tui
        win = self.sections[("read", "write", "cmd").index(kind)]
        rows = max(1, (win.render_info.window_height - 1) if win.render_info else 5)
        width = SIDE_WIDTH - 3
        title, color, items = {
            "read": ("READ", SKY, [(p, n) for p, n in t.read_files.items()]),
            "write": ("WRITTEN", AMBER, list(t.written.items())),
            "cmd": ("COMMANDS", LAVENDER, t.commands),
        }[kind]
        out: list[tuple[str, str]] = [(f"bold fg:{color}", f" ▍{title} "), (f"fg:{MUTED}", f"{len(items)}\n")]
        if not items:
            out.append((f"fg:{MUTED} italic", "  nothing yet"))
            return out
        hidden = max(0, len(items) - rows)
        if hidden:
            rows -= 1
            hidden = len(items) - rows
        shown = items[-rows:]
        if hidden:
            out.append((f"fg:{MUTED}", f"  ⋯ {hidden} earlier\n"))
        for i, it in enumerate(shown):
            nl = "\n" if i < len(shown) - 1 else ""
            if kind == "read":
                path, n = it
                tail = f" ×{n}" if n > 1 else ""
                head, _, name = _clip(path, width - 2 - len(tail)).rpartition("/")
                out += [(f"fg:{MUTED}", "  " + (head + "/" if head else "")), (f"bold fg:{FG}", name), (f"fg:{MUTED}", tail + nl)]
            elif kind == "write":
                path, (adds, dels, new) = it
                stat = ("new " if new else "") + (f"+{adds}" if adds else "") + (f" −{dels}" if dels else "")
                head, _, name = _clip(path, width - 2 - len(stat) - 1).rpartition("/")
                out += [(f"fg:{MUTED}", "  " + (head + "/" if head else "")), (f"bold fg:{FG}", name), (f"fg:{MINT}", " " + stat + nl)]
            else:
                cmd, ok = it
                out += [(f"fg:{MINT if ok else CORAL} bold", " ✓ " if ok else " ✗ "), (f"fg:{FG}", _clip(cmd.replace("\n", " "), width - 3) + nl)]
        return out

    def stop(self) -> None:
        """Leave the alt screen with the conversation still on the real terminal."""
        import sys

        sys.__stdout__.write(self.sink.text)
        sys.__stdout__.flush()
