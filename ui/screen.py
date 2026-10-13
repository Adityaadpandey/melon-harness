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
from rich.text import Text

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

    def __init__(self, text, on_scroll, cursor=None, on_mouse=None) -> None:
        super().__init__(text, show_cursor=False, get_cursor_position=cursor)
        self._on_scroll = on_scroll
        self._on_mouse = on_mouse

    def mouse_handler(self, mouse_event):
        if mouse_event.event_type == MouseEventType.SCROLL_UP:
            self._on_scroll(3)
        elif mouse_event.event_type == MouseEventType.SCROLL_DOWN:
            self._on_scroll(-3)
        elif self._on_mouse:
            return self._on_mouse(mouse_event)
        else:
            return NotImplemented


def copy_to_clipboard(text: str, output=None) -> None:
    """System clipboard through pbcopy / wl-copy / xclip when there is one, and always the
    terminal's own OSC 52 escape too, which also works over ssh."""
    import base64
    import shutil
    import subprocess

    for cmd in (["pbcopy"], ["wl-copy"], ["xclip", "-selection", "clipboard"], ["clip"]):
        if shutil.which(cmd[0]):
            try:
                subprocess.run(cmd, input=text.encode(), timeout=2, check=False)
                break
            except (OSError, subprocess.SubprocessError):
                continue
    if output is not None:
        output.write_raw("\x1b]52;c;" + base64.b64encode(text.encode()).decode() + "\x07")
        output.flush()


def _paint(line: list[tuple[str, str]], a: int, b: int) -> list[tuple[str, str]]:
    """Fragments of one line with the cells from a up to b drawn reversed."""
    out, pos = [], 0
    for style, text in line:
        end = pos + len(text)
        if end <= a or pos >= b:
            out.append((style, text))
        else:
            i, j = max(a, pos) - pos, min(b, end) - pos
            out += [(style, text[:i])] * bool(i) + [(style + " reverse", text[i:j])] + [(style, text[j:])] * bool(j < len(text))
        pos = end
    return out


class Screen:
    def __init__(self, tui: TUI, console: Console, title=lambda: "") -> None:
        self.tui, self.console, self._title = tui, console, title
        self._cache: tuple[int, Any, int, list] = (-1, [], 0, [])  # (text length, formatted text, last line, lines)
        self.sel: tuple[tuple[int, int], tuple[int, int]] | None = None  # (line, column) pairs, anchor then head
        self._dragging = False
        self._click = (0.0, None, 0)  # when, where and how many times the last press repeated
        self._copied_at = -99.0
        self.anchor: int | None = None  # line kept in view while scrolled back; None follows new output
        self.sink = Sink()
        self.kb = KeyBindings()
        self._status_cache = (0.0, "", 0)  # (rendered at, ansi, rows)
        self.mouse_on = True  # F2 turns mouse capture off so the terminal's own selection works
        self._last_move = 0.0
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

        @self.kb.add("escape", eager=True, filter=Condition(lambda: self.sel is not None))
        def _clear_sel(event):
            self.sel = None

        @self.kb.add("f2")
        def _toggle_mouse(event):
            self.mouse_on = not self.mouse_on
            self.sel, self._dragging = None, False

        @self.kb.add("c-home")
        def _top(event):
            self.anchor = 0

        @self.kb.add("c-end")
        def _bottom(event):
            self.anchor = None

        # the Window keeps the cursor line visible, so parking the cursor on the last line follows
        # the output and parking it higher up holds the view still while new lines arrive below
        self.main = Window(_Pane(self._transcript, self.scroll, self._cursor, self._mouse_main), wrap_lines=False, height=Dimension(weight=1))
        self.status = Window(_Pane(self._status, self.scroll, on_mouse=self._mouse_elsewhere), height=lambda: Dimension.exact(self._status_render()[1]))
        self.sections = [Window(_Pane(lambda k=k: self._section(k), self.scroll, on_mouse=self._mouse_elsewhere), wrap_lines=False, style="bg:#151826", height=Dimension(weight=1)) for k in ("read", "write", "cmd")]
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
            flat = to_formatted_text(ANSI(body))
            raw: list[list[tuple[str, str]]] = [[]]
            for style, frag, *_ in flat:
                for k, piece in enumerate(frag.split("\n")):
                    if k:
                        raw.append([])
                    if piece:
                        raw[-1].append((style, piece))
            # several blank lines in a row (every block adds its own gap) read as one
            lines: list[list[tuple[str, str]]] = []
            for ln in raw:
                blank = not "".join(t for _, t in ln).strip()
                if blank and lines and not "".join(t for _, t in lines[-1]).strip():
                    continue
                lines.append(ln)
            flat = [f for i, ln in enumerate(lines) for f in (ln + ([("", "\n")] if i < len(lines) - 1 else []))]
            self._cache = (len(text), flat, len(lines) - 1, lines)
        return self._cache

    # -- selecting and copying -----------------------------------------
    def _plain(self, i: int) -> str:
        lines = self._formatted()[3]
        return "".join(t for _, t in lines[i]) if 0 <= i < len(lines) else ""

    def _span(self, i: int, a: tuple[int, int], b: tuple[int, int]) -> tuple[int, int]:
        n = len(self._plain(i))
        return (a[1] if i == a[0] else 0), (min(n, b[1] + 1) if i == b[0] else n)

    def _selected_text(self) -> str:
        if not self.sel:
            return ""
        a, b = sorted(self.sel)
        rows = []
        for i in range(a[0], b[0] + 1):
            lo, hi = self._span(i, a, b)
            rows.append(self._plain(i)[lo:hi].rstrip())
        return "\n".join(rows).strip("\n")

    def _mouse_main(self, ev):
        """Drag to select, double-click for a word, triple-click for a line. Copies on release."""
        pos = (max(0, ev.position.y), max(0, ev.position.x))
        t = ev.event_type
        if t == MouseEventType.MOUSE_DOWN:
            when, where, n = self._click
            n = n + 1 if (time.monotonic() - when < 0.45 and where == pos) else 1
            self._click = (time.monotonic(), pos, n)
            line = self._plain(pos[0])
            if n == 2:
                lo, hi = pos[1], pos[1]
                while lo > 0 and lo - 1 < len(line) and not line[lo - 1].isspace():
                    lo -= 1
                while hi + 1 < len(line) and not line[hi + 1].isspace():
                    hi += 1
                self.sel = ((pos[0], lo), (pos[0], hi))
            elif n >= 3:
                self.sel = ((pos[0], 0), (pos[0], max(0, len(line.rstrip()) - 1)))
            else:
                self.sel = (pos, pos)
            self._dragging = n == 1
            if n > 1:
                self._copy()
        elif t == MouseEventType.MOUSE_MOVE and self._dragging and self.sel:
            self.sel = (self.sel[0], pos)
            self._last_move = time.monotonic()
        elif t == MouseEventType.MOUSE_UP and self._dragging:
            self._dragging = False
            if self.sel and self.sel[0] == self.sel[1]:
                self.sel = None  # a plain click selects nothing
            else:
                self.sel = (self.sel[0], pos) if self.sel else None
                self._copy()
        else:
            return NotImplemented

    def _mouse_elsewhere(self, ev):
        """A drag that ends over the side panel or input box still finishes and copies."""
        if self._dragging and ev.event_type == MouseEventType.MOUSE_UP:
            self._dragging = False
            self._copy()
            return None
        return NotImplemented

    def _copy(self) -> None:
        text = self._selected_text()
        if text:
            copy_to_clipboard(text, get_app().output)
            self._copied_at = time.monotonic()

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
        right = "scrolled back · PageDown / ctrl+end to return  " if self.anchor is not None else "drag to copy · F2 native select  "
        if not self.mouse_on:
            right = "native select on (wheel off) · F2 to go back  "
        if time.monotonic() - self._copied_at < 2:
            right = "✓ copied to clipboard  "
        pad = " " * max(1, cols - len(left) - len(right))
        busy = self.tui._turn_active
        return [(f"bold fg:{ACCENT}", f"  =^{self._face()}^="), (f"bold fg:{LAVENDER if busy else FG}", "  mew"),
                (f"fg:{MUTED}", f"  ·  {self._title()}{pad}"), (f"bold fg:{MINT}" if time.monotonic() - self._copied_at < 2 else f"fg:{AMBER}" if self.anchor is not None else "fg:#4a4f73", right)]

    # -- panes ---------------------------------------------------------
    def _transcript(self):
        self.console.width = self._main_width(_size()[0])  # follows terminal resizes
        if self._dragging and time.monotonic() - self._last_move > 0.8 and self.sel and self.sel[0] != self.sel[1]:
            self._dragging = False  # the release went somewhere we never heard about: finish anyway
            self._copy()
        if not self.sel:
            return self._formatted()[1]
        a, b = sorted(self.sel)
        out: list[tuple[str, str]] = []
        for i, line in enumerate(self._formatted()[3]):
            if a[0] <= i <= b[0]:
                out += _paint(line, *self._span(i, a, b))
            else:
                out += line
            out.append(("", "\n"))
        return out

    def _status_render(self) -> tuple[str, int]:
        """(ansi, rows) for the working indicator. The height comes from here too: a window that
        starts at height 0 is never drawn, so it could never grow to show anything."""
        at, ansi, rows = self._status_cache
        if time.monotonic() - at < 0.05:
            return ansi, rows
        import ui.prompt as prompt_mod

        asking = prompt_mod.ACTIVE.question if prompt_mod.ACTIVE else None
        if not self.tui._turn_active and not asking:
            self._status_cache = (time.monotonic(), "", 0)
            return "", 0
        c = self._status_console
        c.width = self._main_width(_size()[0])
        c.file = io.StringIO()
        if self.tui._turn_active:
            c.print(self.tui._renderable())
        if asking:
            q, opts = asking
            c.print(Text.assemble((" ? ", f"bold {LAVENDER}"), (q, "bold")))
            for i, opt in enumerate(opts, 1):
                c.print(Text.assemble((f"   [{i}] ", f"bold {CORAL if i == len(opts) else ACCENT}"), (opt, FG), ("  (enter)" if i == 1 else "", MUTED)))
        out = c.file.getvalue().rstrip("\n") + "\n" + self._scanner(c.width)
        self._status_cache = (time.monotonic(), out, out.count("\n") + 1)
        return out, out.count("\n") + 1

    def _status(self):
        return ANSI(self._status_render()[0])

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

    def _totals(self, kind: str, items) -> list[tuple[str, str]]:
        """WRITTEN gets the grand total of lines added and removed next to its count."""
        if kind != "write" or not items:
            return []
        adds, dels = sum(v[0] for _, v in items), sum(v[1] for _, v in items)
        return [("", "  "), (f"fg:{MINT}", f"+{adds}"), (f"fg:{CORAL}", f" −{dels}")]

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
        out: list[tuple[str, str]] = [("", " "), (f"bold fg:#14161f bg:{color}", f" {title} "), (f"bold fg:{color}", f" {len(items)}"), *self._totals(kind, items), ("", "\n")]
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
                cells = min(6, max(1, round(math.log2(total + 1) * 1.2)))  # bar length grows slowly with change size
                a_c = round(cells * adds / total) if dels else cells
                nums = [(f"fg:{MINT}{glow}", f"+{adds}" if adds else ""), (f"fg:{CORAL}{glow}", f" −{dels}" if dels else "")]
                numw = len(nums[0][1]) + len(nums[1][1])
                tag = " new" if new else ""
                head, _, name = _clip(path, width - 2 - numw - cells - len(tag) - 2).rpartition("/")
                out += [(f"fg:{ACCENT}{glow}" if fresh else f"fg:{MUTED}", " ●" if fresh else "  "), (f"fg:{MUTED}{glow}", (head + "/" if head else "")),
                        (f"bold fg:{FG}{glow}", name), (f"fg:{AMBER}", tag + " "), *nums, ("", " "),
                        (f"fg:{MINT}{glow}", "█" * a_c), (f"fg:{CORAL}{glow}", "█" * (cells - a_c)), ("", nl)]
            else:
                cmd, ok = it
                out += [(f"fg:{MINT if ok else CORAL} bold{glow}", " ✓ " if ok else " ✗ "), (f"fg:{FG}{glow}", _clip(cmd.replace("\n", " "), width - 3) + nl)]
        return out

    def stop(self) -> None:
        """Leave the alt screen with the conversation still on the real terminal."""
        import sys

        sys.__stdout__.write(self.sink.text)
        sys.__stdout__.flush()
