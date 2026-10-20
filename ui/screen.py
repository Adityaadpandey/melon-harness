"""Full-screen layout: the conversation on the left, a side panel on the right listing the files
read, the files written and the commands run, then the status line and the input box underneath.

Everything the agent prints still goes through the shared rich console. In full-screen mode that
console writes into a Sink instead of the terminal, and the transcript pane shows the tail of it."""
from __future__ import annotations

import io
import math
import os
import re
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

from ui.palette import BG_BOX, BG_GLOW, BG_PANEL, FAINT, INK, RULE
from ui.tui import _mix, ACCENT, agent_stats, agent_title, AGENT_THEME, AMBER, CORAL, FG, LAVENDER, MINT, MUTED, SKY, TUI, redirect_console

SIDE_WIDTH = 38
MIN_COLUMNS = 100  # narrower terminals get the conversation only
MAX_LINES = 3000
MARGIN = 2


class Sink:
    """File-like target for the console: keeps what was printed and nudges a redraw."""

    def __init__(self) -> None:
        self.text = ""
        self.nl = 0  # newlines in text, counted as they arrive
        self.gen = 0  # bumps when the oldest lines are dropped, so cached parsing is thrown away
        self.on_write = lambda: None

    def write(self, s: str) -> int:
        self.text += s
        self.nl += s.count("\n")
        if self.nl > MAX_LINES * 1.2:  # keep memory bounded on very long sessions
            self.text = "\n".join(self.text.split("\n")[-MAX_LINES:])
            self.nl = self.text.count("\n")
            self.gen += 1
        self.on_write()
        return len(s)

    def flush(self) -> None:
        pass

    def isatty(self) -> bool:
        return True


# what rich draws at the start of a line that is not the text: " ▎ ", " ◆ ", " │ ", " ▍ ", "▌ " or a box corner
_DECOR = re.compile(r"^(?: [▎◆│▍] ?|▌ |[│┃╭╰]\s?)")


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
        super().__init__(self._grab, show_cursor=False, get_cursor_position=cursor)
        self._text, self._src = text, None
        self._last: tuple = (None, None, None)  # (fragments, width and cursor, content)
        self._wide: tuple = (None, 0)  # (fragments, preferred width)
        self._on_scroll = on_scroll
        self._on_mouse = on_mouse

    def _grab(self):
        self._src = self._text()
        return self._src

    # The transcript hands back the very same list until new output lands. prompt_toolkit would still
    # re-split and hash the whole session on every redraw (~40ms at 20k lines, so a spinner ticking at
    # 10fps kept a core busy); when the list is the one we laid out last time, reuse that layout.
    def create_content(self, width, height):
        self._get_formatted_text_cached()  # runs the text callable once for this render, so _src is current
        key = (width, self.get_cursor_position() if self.get_cursor_position else None)
        if self._last[0] is self._src and self._last[1] == key:
            return self._last[2]
        content = super().create_content(width, height)
        self._last = (self._src, key, content)
        return content

    def preferred_width(self, max_available_width):
        self._get_formatted_text_cached()
        if self._wide[0] is not self._src:
            self._wide = (self._src, super().preferred_width(max_available_width))
        return self._wide[1]

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
        self._gen, self._done_len, self._done, self._done_flat = 0, 0, [], []  # finished lines already parsed, and how much text they cover
        self.sel: tuple[tuple[int, int], tuple[int, int]] | None = None  # (line, column) pairs, anchor then head
        self._dragging = False
        self._side_seen = False  # the side panel stays hidden until the agent reads, writes, runs or delegates something
        self._edge = 0  # while dragging: 1 = pointer at/above the top (scroll back), -1 = at/below the bottom, 0 = inside
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
        self.status = Window(_Pane(self._status, self.scroll, on_mouse=lambda ev: self._mouse_beyond(ev, -1)), height=lambda: Dimension.exact(self._status_render()[1]))
        self.sections = [Window(_Pane(lambda k=k: self._section(k), self.scroll, on_mouse=self._mouse_elsewhere), wrap_lines=False, style=f"bg:{BG_PANEL}", height=Dimension(weight=1)) for k in ("agents", "read", "write", "cmd")]
        rule = lambda: Window(height=1, char="─", style=f"fg:{RULE} bg:{BG_PANEL}")
        # only sections that have something are drawn, so a quiet panel is not four empty boxes
        shown = []
        for k, w in enumerate(self.sections):
            shown += [ConditionalContainer(rule(), filter=Condition(lambda k=k: self._has(k) and any(self._has(j) for j in range(k)))),
                      ConditionalContainer(w, filter=Condition(lambda k=k: self._has(k)))]
        side = VSplit([
            Window(width=1, char="│", style=f"fg:{RULE}"),
            HSplit(shown, width=SIDE_WIDTH, style=f"bg:{BG_PANEL}"),
        ])
        wide = Condition(lambda: self._side_on(_size()[0]))
        header = Window(_Pane(self._header, self.scroll, on_mouse=lambda ev: self._mouse_beyond(ev, 1)), height=1, style=f"bg:{BG_BOX}")
        margin = Window(width=MARGIN)
        self.body = HSplit([
            header,
            VSplit([margin, self.main, ConditionalContainer(side, filter=wide)], height=Dimension(min=4, weight=100)),
            VSplit([margin, self.status]),
        ])
        self.sink.on_write = self._poke

    def clear(self) -> None:
        """/clear: wipe the transcript and everything cached from it."""
        self.sink.text, self.sink.nl = "", 0
        self.sink.gen += 1
        self._cache = (-1, [], 0, [])
        self.sel, self.anchor, self._dragging, self._edge = None, None, False, 0

    # -- layout helpers ------------------------------------------------
    def _has(self, k: int) -> bool:
        t = self.tui
        return bool((t.agents or t.agent_log, t.read_files, t.written, t.commands)[k])

    def _side_on(self, cols: int) -> bool:
        t = self.tui
        self._side_seen = self._side_seen or bool(t.read_files or t.written or t.commands or t.agents or t.agent_log)
        return cols >= MIN_COLUMNS and self._side_seen

    def _main_width(self, cols: int) -> int:
        return max(40, cols - MARGIN - (SIDE_WIDTH + 1 if self._side_on(cols) else 0) - 1)

    def _main_rows(self) -> int:
        info = self.main.render_info
        return info.window_height if info else max(5, _size()[1] - 8)

    def _poke(self) -> None:
        try:
            get_app().invalidate()
        except Exception:
            pass

    def _formatted(self):
        """The transcript as formatted text. Finished lines are parsed once and kept; only the line
        still being written is parsed again, so a long session costs the same per redraw as a short one."""
        text = self.sink.text
        if len(text) == self._cache[0] and self.sink.gen == self._gen:
            return self._cache
        if self.sink.gen != self._gen or len(text) < self._done_len:
            self._gen, self._done_len, self._done, self._done_flat = self.sink.gen, 0, [], []  # the head was trimmed: start over
        cut = text.rfind("\n") + 1
        if cut > self._done_len:
            new = self._split(text[self._done_len:cut])
            self._done += new
            self._done_flat += [f for ln in new for f in (*ln, ("", "\n"))]  # every finished line ends in a newline
            self._done_len = cut
        tail = self._split(text[cut:]) if cut < len(text) else []
        if tail:
            lines, flat = self._done + tail, self._done_flat + tail[0]
        else:  # nothing half-written: the last finished line is the end, without a trailing newline
            lines, flat = self._done, self._done_flat[:-1]
        self._cache = (len(text), flat, len(lines) - 1, lines)
        return self._cache

    def _split(self, chunk: str) -> list[list[tuple[str, str]]]:
        """Styled lines for a piece of console output, with runs of blank lines folded into one
        (every block adds its own gap). A piece ending in a newline has no half-written line after it."""
        raw: list[list[tuple[str, str]]] = [[]]
        for style, frag, *_ in to_formatted_text(ANSI(chunk[:-1] if chunk.endswith("\n") else chunk)):
            for k, piece in enumerate(frag.split("\n")):
                if k:
                    raw.append([])
                if piece:
                    raw[-1].append((style, piece))
        out: list[list[tuple[str, str]]] = []
        for ln in raw:
            prev = self._done[-1] if not out and self._done else out[-1] if out else None
            if not "".join(t for _, t in ln).strip() and prev is not None and not "".join(t for _, t in prev).strip():
                continue
            out.append(ln)
        return out

    # -- selecting and copying -----------------------------------------
    def _plain(self, i: int) -> str:
        lines = self._formatted()[3]
        return "".join(t for _, t in lines[i]) if 0 <= i < len(lines) else ""

    def _span(self, i: int, a: tuple[int, int], b: tuple[int, int]) -> tuple[int, int]:
        n = len(self._plain(i))
        return (a[1] if i == a[0] else 0), (min(n, b[1] + 1) if i == b[0] else n)

    def _body(self, i: int) -> tuple[int, int]:
        """Columns of line i that hold the text itself, without the rail glyph on the left
        (" ▎ ", " ◆ ", " │ ") or a panel's side border on the right. Copying skips the rest."""
        line = self._plain(i)
        m = _DECOR.match(line)
        lo = m.end() if m else 0
        hi = len(line.rstrip())
        if hi > lo and line[hi - 1] in "│┃":
            hi = len(line[:hi - 1].rstrip())
        return lo, max(lo, hi)

    def all_text(self) -> str:
        """The whole transcript as plain text, for /copy all."""
        rows = []
        for i in range(len(self._formatted()[3])):
            lo, hi = self._body(i)
            rows.append(self._plain(i)[lo:hi])
        return "\n".join(rows).strip()

    def _selected_text(self) -> str:
        if not self.sel:
            return ""
        a, b = sorted(self.sel)
        rows = []
        for i in range(a[0], b[0] + 1):
            lo, hi = self._span(i, a, b)
            top, end = self._body(i)
            rows.append(self._plain(i)[max(lo, top):min(hi, end)].rstrip())
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
            info = self.main.render_info
            if info:  # parked on the first or last visible row: _transcript keeps scrolling and growing the selection
                rel = pos[0] - info.vertical_scroll
                self._edge = 1 if rel <= 0 else -1 if rel >= info.window_height - 1 else 0
        elif t == MouseEventType.MOUSE_UP and self._dragging:
            self._dragging, self._edge = False, 0
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
            self._dragging, self._edge = False, 0
            self._copy()
            return None
        return NotImplemented

    def _mouse_beyond(self, ev, edge: int):
        """The pointer left the transcript while dragging: above it (header) scrolls back, below it (status/input) scrolls forward."""
        if self._dragging and ev.event_type == MouseEventType.MOUSE_MOVE:
            self._edge, self._last_move = edge, time.monotonic()
            return None
        return self._mouse_elsewhere(ev)

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

    def frame(self):
        """What would look different on the next redraw. While something moves (a turn, a question,
        a drag) it's the clock, so every tick redraws; when idle it only changes when the cat blinks
        or a flash or glow wears off, so a quiet screen isn't redrawn ten times a second for nothing."""
        import ui.prompt as prompt_mod

        now, t = time.monotonic(), self.tui
        if t._turn_active or t.agents or self._dragging or (prompt_mod.ACTIVE and prompt_mod.ACTIVE.question):
            return now
        return (self._face(), self._title(), now - self._copied_at < 2, any(now - v < 2 for v in t.touched.values()))

    def _header(self):
        cols = _size()[0]
        left = f"  =^{self._face()}^=  melon  ·  {self._title()}"
        right = "scrolled back · PageDown / ctrl+end to return  " if self.anchor is not None else "drag to copy · /copy all · F2 native select  "
        if not self.mouse_on:
            right = "native select on (wheel off) · F2 to go back  "
        if time.monotonic() - self._copied_at < 2:
            right = "✓ copied to clipboard  "
        pad = " " * max(1, cols - len(left) - len(right))
        busy = self.tui._turn_active
        return [(f"bold fg:{ACCENT}", f"  =^{self._face()}^="), (f"bold fg:{LAVENDER if busy else FG}", "  melon"),
                (f"fg:{MUTED}", f"  ·  {self._title()}{pad}"), (f"bold fg:{MINT}" if time.monotonic() - self._copied_at < 2 else f"fg:{AMBER}" if self.anchor is not None else f"fg:{FAINT}", right)]

    # -- panes ---------------------------------------------------------
    def _transcript(self):
        self.console.width = self._main_width(_size()[0])  # follows terminal resizes
        self.tui.side_panel = self._side_on(_size()[0])
        if self._dragging and self._edge and self.sel:  # holding the pointer at an edge: keep scrolling and stretch the selection with it
            self.scroll(2 * self._edge)
            info = self.main.render_info
            if info:
                row = info.vertical_scroll if self._edge > 0 else info.vertical_scroll + info.window_height - 1
                self.sel = (self.sel[0], (max(0, row), 0 if self._edge > 0 else 10**6))
        elif self._dragging and time.monotonic() - self._last_move > 0.8 and self.sel and self.sel[0] != self.sel[1]:
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
                label, _, why = opt.partition(" — ")  # a choice may carry a one-line explanation after the dash
                c.print(Text.assemble((f"   [{i}] ", f"bold {CORAL if i == len(opts) else ACCENT}"), (label, f"bold {FG}" if why else FG), (f"  {why}" if why else "", MUTED), ("  ⏎" if i == 1 else "", MUTED)))
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
            r, g, b = (int(_mix(RULE, ACCENT, glow)[k:k + 2], 16) for k in (1, 3, 5))
            cells.append(f"\x1b[38;2;{r};{g};{b}m{'━' if glow > 0.05 else '─'}")
        return "".join(cells) + "\x1b[0m"

    def _totals(self, kind: str, items) -> list[tuple[str, str]]:
        """WRITTEN gets the grand total of lines added and removed next to its count."""
        if kind != "write" or not items:
            return []
        adds, dels = sum(v[0] for _, v in items), sum(v[1] for _, v in items)
        return [("", "  "), (f"fg:{MINT}", f"+{adds}"), (f"fg:{CORAL}", f" −{dels}")]

    def _agents_section(self, rows: int) -> list[tuple[str, str]]:
        """Sub-agents: running ones with their current tool, finished ones with time and tool count."""
        t = self.tui
        now = time.monotonic()
        width = SIDE_WIDTH - 3
        count = len(t.agents) + len(t.agent_log)
        out: list[tuple[str, str]] = [("", " "), (f"bold fg:{INK} bg:{ACCENT}", " AGENTS "), (f"bold fg:{ACCENT}", f" {len(t.agents)} running" if t.agents else f" {count}"), ("", "\n")]
        if not count:
            return out + [(f"fg:{MUTED} italic", "\n   sub-agents show up here live")]
        lines: list[list[tuple[str, str]]] = []
        for a in t.agents.values():
            lines.append([(f"fg:{ACCENT}", " ◆ "), (f"bold fg:{FG}", agent_title(a["agent"])), (f"fg:{MUTED}", f"  {agent_stats(a, now - a['started'])}")])
            lines.append([(f"fg:{MUTED}", "   ╰ "), (f"fg:{FG}", _clip(a["recent"][-1] if a["recent"] else "starting…", width - 5))])
        for a in t.agent_log:
            lines.append([(f"fg:{MINT if a['ok'] else CORAL}", " ✓ " if a["ok"] else " ✗ "), (f"fg:{FG}", agent_title(a["agent"])), (f"fg:{MUTED}", f"  {agent_stats(a, a['seconds'])}")])
        hidden = max(0, len(lines) - rows)
        if hidden:
            out.append((f"fg:{MUTED}", f"  ⋯ {hidden} more\n"))
        for i, ln in enumerate(lines[hidden:]):
            out += ln + ([("", "\n")] if i < len(lines) - hidden - 1 else [])
        return out

    def _section(self, kind: str):
        t = self.tui
        if kind == "agents":
            win = self.sections[0]
            return self._agents_section(max(1, (win.render_info.window_height - 2) if win.render_info else 4))
        win = self.sections[("agents", "read", "write", "cmd").index(kind)]
        rows = max(1, (win.render_info.window_height - 1) if win.render_info else 5)
        width = SIDE_WIDTH - 3
        title, color, items = {
            "read": ("READ", SKY, [(p, n) for p, n in t.read_files.items()]),
            "write": ("WRITTEN", AMBER, list(t.written.items())),
            "cmd": ("COMMANDS", LAVENDER, t.commands),
        }[kind]
        out: list[tuple[str, str]] = [("", " "), (f"bold fg:{INK} bg:{color}", f" {title} "), (f"bold fg:{color}", f" {len(items)}"), *self._totals(kind, items), ("", "\n")]
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
            glow = f" bg:{BG_GLOW}" if fresh else ""
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
