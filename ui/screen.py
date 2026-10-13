"""Full-screen layout: the conversation on the left, a side panel on the right listing the files
read, the files written and the commands run, then the status line and the input box underneath.

Everything the agent prints still goes through the shared rich console. In full-screen mode that
console writes into a Sink instead of the terminal, and the transcript pane shows the tail of it."""
from __future__ import annotations

import io
import math
import os
import time
from typing import Any

from prompt_toolkit.application import get_app
from prompt_toolkit.filters import Condition
from prompt_toolkit.data_structures import Point
from prompt_toolkit.formatted_text import ANSI, to_formatted_text
from prompt_toolkit.key_binding import KeyBindings
from prompt_toolkit.layout import ConditionalContainer, HSplit, VSplit, Window
from prompt_toolkit.layout.controls import FormattedTextControl
from prompt_toolkit.layout.dimension import Dimension
from prompt_toolkit.mouse_events import MouseEventType
from rich.console import Console

from ui.tui import _mix, ACCENT, AGENT_THEME, AMBER, CORAL, FG, LAVENDER, MINT, MUTED, SKY, TUI, redirect_console

SIDE_WIDTH = 38
MIN_COLUMNS = 100  # narrower terminals get the conversation only
MAX_LINES = 3000
MARGIN = 2


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
    """Text that scrolls the transcript when the mouse wheel moves over it."""

    def __init__(self, text, on_scroll, cursor=None) -> None:
        super().__init__(text, show_cursor=False, get_cursor_position=cursor)
        self._on_scroll = on_scroll

    def mouse_handler(self, mouse_event):
        if mouse_event.event_type == MouseEventType.SCROLL_UP:
            self._on_scroll(3)
        elif mouse_event.event_type == MouseEventType.SCROLL_DOWN:
            self._on_scroll(-3)
        else:
            return NotImplemented


class Screen:
    def __init__(self, tui: TUI, console: Console, title=lambda: "") -> None:
        self.tui, self.console, self._title = tui, console, title
        self._cache: tuple[int, Any, int] = (-1, [], 0)  # (text length, formatted text, line count)
        self.anchor: int | None = None  # line kept in view while scrolled back; None follows new output
        self.sink = Sink()
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

        @self.kb.add("c-home")
        def _top(event):
            self.anchor = 0

        @self.kb.add("c-end")
        def _bottom(event):
            self.anchor = None

        # the Window keeps the cursor line visible, so parking the cursor on the last line follows
        # the output and parking it higher up holds the view still while new lines arrive below
        self.main = Window(_Pane(self._transcript, self.scroll, self._cursor), wrap_lines=False, height=Dimension(weight=1))
        self.status = Window(_Pane(self._status, self.scroll), height=lambda: Dimension.exact(self._status_height))
        self.sections = [Window(_Pane(lambda k=k: self._section(k), self.scroll), wrap_lines=False, style="bg:#151826", height=Dimension(weight=1)) for k in ("read", "write", "cmd")]
        rule = lambda: Window(height=1, char="─", style="fg:#3a3f5c bg:#151826")
        side = VSplit([
            Window(width=1, char="│", style="fg:#3a3f5c"),
            HSplit([self.sections[0], rule(), self.sections[1], rule(), self.sections[2]], width=SIDE_WIDTH, style="bg:#151826"),
        ])
        wide = Condition(lambda: _size()[0] >= MIN_COLUMNS)
        header = Window(_Pane(self._header, self.scroll), height=1, style="bg:#1d2033")
        margin = Window(width=MARGIN)
        self.body = HSplit([
            header,
            VSplit([margin, self.main, ConditionalContainer(side, filter=wide)], height=Dimension(min=4, weight=100)),
            VSplit([margin, self.status]),
        ])
        self.sink.on_write = self._poke

    # -- layout helpers ------------------------------------------------
    def _main_width(self, cols: int) -> int:
        return max(40, cols - MARGIN - (SIDE_WIDTH + 1 if cols >= MIN_COLUMNS else 0) - 1)

    def _main_rows(self) -> int:
        info = self.main.render_info
        return info.window_height if info else max(5, _size()[1] - 8)

    def _poke(self) -> None:
        try:
            get_app().invalidate()
        except Exception:
            pass

    def _formatted(self):
        """The transcript as formatted text, parsed again only when something new was printed."""
        text = self.sink.text
        if len(text) != self._cache[0]:
            body = text[:-1] if text.endswith("\n") else text
            self._cache = (len(text), to_formatted_text(ANSI(body)), body.count("\n"))
        return self._cache

    def scroll(self, delta: int) -> None:
        """Positive goes back in time. Scrolling past the newest line goes back to following."""
        last = self._formatted()[2]
        at = last if self.anchor is None else self.anchor
        at = max(0, min(last, at - delta))
        self.anchor = None if at >= last else at

    def _cursor(self) -> Point:
        last = self._formatted()[2]
        return Point(0, last if self.anchor is None else min(self.anchor, last))

    def _face(self) -> str:
        """The cat blinks when idle and widens its eyes while the agent is working."""
        now = time.monotonic()
        if self.tui._turn_active:
            return ("O.O", "o.o", "O.O", "o.O", "O.o")[int(now * 3) % 5]
        return "-.-" if int(now * 2) % 9 == 0 else "o.o"

    def _header(self):
        cols = _size()[0]
        left = f"  =^{self._face()}^=  mew  ·  {self._title()}"
        right = "scrolled back · PageDown / ctrl+end to return  " if self.anchor is not None else "PageUp/Down scroll  "
        pad = " " * max(1, cols - len(left) - len(right))
        busy = self.tui._turn_active
        return [(f"bold fg:{ACCENT}", f"  =^{self._face()}^="), (f"bold fg:{LAVENDER if busy else FG}", "  mew"),
                (f"fg:{MUTED}", f"  ·  {self._title()}{pad}"), (f"fg:{AMBER}" if self.anchor is not None else "fg:#4a4f73", right)]

    # -- panes ---------------------------------------------------------
    def _transcript(self):
        self.console.width = self._main_width(_size()[0])  # follows terminal resizes
        return self._formatted()[1]

    def _status(self):
        if not self.tui._turn_active:
            self._status_height = 0
            return ""
        c = self._status_console
        c.width = self._main_width(_size()[0])
        c.file = io.StringIO()
        c.print(self.tui._renderable())
        out = c.file.getvalue().rstrip("\n") + "\n" + self._scanner(c.width)
        self._status_height = out.count("\n") + 1
        return ANSI(out)

    def _scanner(self, width: int) -> str:
        """A thin light that sweeps back and forth above the input while the agent works."""
        w = max(10, min(width, 120))
        pos = (math.sin(time.monotonic() * 2.2) + 1) / 2 * (w - 1)
        cells = []
        for i in range(w):
            glow = max(0.0, 1 - abs(i - pos) / 7)
            r, g, b = (int(_mix("#2a2e48", ACCENT, glow)[k:k + 2], 16) for k in (1, 3, 5))
            cells.append(f"\x1b[38;2;{r};{g};{b}m{'━' if glow > 0.05 else '─'}")
        return "".join(cells) + "\x1b[0m"

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
        out: list[tuple[str, str]] = [("", " "), (f"bold fg:#14161f bg:{color}", f" {title} "), (f"bold fg:{color}", f" {len(items)}\n")]
        if not items:
            hint = {"read": "files the agent opens", "write": "edits show up with a +/− bar", "cmd": "builds, tests and scripts"}[kind]
            out.append((f"fg:{MUTED} italic", f"\n   {hint}"))
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
            key = {"read": "read:" + str(it[0]), "write": "write:" + str(it[0]), "cmd": f"cmd:{items.index(it)}"}[kind]
            fresh = time.monotonic() - t.touched.get(key, -99) < 2.0  # glows for a moment after the agent touches it
            glow = f" bg:#2a2140" if fresh else ""
            if kind == "read":
                path, n = it
                tail = f" ×{n}" if n > 1 else ""
                head, _, name = _clip(path, width - 2 - len(tail)).rpartition("/")
                out += [(f"fg:{ACCENT}{glow}" if fresh else f"fg:{MUTED}", " ●" if fresh else "  "), (f"fg:{MUTED}{glow}", (head + "/" if head else "")), (f"bold fg:{FG}{glow}", name), (f"fg:{MUTED}", tail + nl)]
            elif kind == "write":
                path, (adds, dels, new) = it
                total = max(1, adds + dels)
                cells = min(8, max(1, round(math.log2(total + 1) * 1.4)))  # bar length grows slowly with change size
                a_c = round(cells * adds / total) if dels else cells
                bar = [(f"fg:{MINT}{glow}", "█" * a_c), (f"fg:{CORAL}{glow}", "█" * (cells - a_c))]
                stat = " new" if new else ""
                head, _, name = _clip(path, width - 2 - cells - len(stat) - 2).rpartition("/")
                out += [(f"fg:{ACCENT}{glow}" if fresh else f"fg:{MUTED}", " ●" if fresh else "  "), (f"fg:{MUTED}{glow}", (head + "/" if head else "")), (f"bold fg:{FG}{glow}", name), (f"fg:{AMBER}", stat + " "), *bar, ("", nl)]
            else:
                cmd, ok = it
                out += [(f"fg:{MINT if ok else CORAL} bold{glow}", " ✓ " if ok else " ✗ "), (f"fg:{FG}{glow}", _clip(cmd.replace("\n", " "), width - 3) + nl)]
        return out

    def stop(self) -> None:
        """Leave the alt screen with the conversation still on the real terminal."""
        import sys

        sys.__stdout__.write(self.sink.text)
        sys.__stdout__.flush()
