//! The full-screen layout: the conversation on the left, a side panel on the right listing the files read, the files
//! written and the commands run, then the status line and the input box underneath.
//!
//! Everything the agent prints goes through `sink`; this file draws the tail of it. It owns no terminal: `fullscreen.rs`
//! feeds it key and mouse events and hands it a ratatui frame to draw into.
use super::ansi::{self, Line as ALine};
use super::editor::Editor;
use super::input::Completions;
use super::sink;
use super::style::*;
use super::tui::{agent_title, SideData, Tui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RLine, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

pub const SIDE_WIDTH: usize = 38;
pub const MIN_COLUMNS: usize = 100; // narrower terminals get the conversation only
pub const MARGIN: usize = 2;

fn hx(h: &str) -> Color {
    let h = h.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(255);
    Color::Rgb(p(0), p(2), p(4))
}

fn fg(h: &str) -> Style {
    Style::default().fg(hx(h))
}

fn bold_fg(h: &str) -> Style {
    fg(h).add_modifier(Modifier::BOLD)
}

fn chip_style(h: &str) -> Style {
    Style::default().fg(hx(INK)).bg(hx(h)).add_modifier(Modifier::BOLD)
}

fn sp(style: Style, t: impl Into<String>) -> Span<'static> {
    Span::styled(t.into(), style)
}

fn to_rline(l: &ALine) -> RLine<'static> {
    RLine::from(l.iter().map(|(s, t)| Span::styled(t.clone(), *s)).collect::<Vec<_>>())
}

/// Keep the end of a long path, where the file name is.
fn clip(path: &str, width: usize) -> String {
    let n = path.chars().count();
    if n <= width { path.to_string() } else { format!("…{}", path.chars().skip(n - (width.saturating_sub(1))).collect::<String>()) }
}

// ---------------------------------------------------------------------------------------------------- transcript

/// The sink text split into styled lines. Finished lines are parsed once and kept; only the line still being written
/// is parsed again, so a long session costs the same per redraw as a short one.
#[derive(Default)]
struct Transcript {
    generation: u64,
    done_len: usize,
    done: Vec<ALine>,
    tail: Vec<ALine>,
}

fn blank(l: &ALine) -> bool {
    ansi::plain(l).trim().is_empty()
}

/// Styled lines for a piece of output, with runs of blank lines folded into one (every block adds its own gap).
fn split(chunk: &str, prev: Option<&ALine>) -> Vec<ALine> {
    let mut out: Vec<ALine> = vec![];
    for ln in ansi::parse(chunk.strip_suffix('\n').unwrap_or(chunk)) {
        let after_blank = out.last().or(prev).map_or(false, blank);
        if blank(&ln) && after_blank {
            continue;
        }
        out.push(ln);
    }
    out
}

impl Transcript {
    fn sync(&mut self) {
        sink::with(|s| {
            if s.generation != self.generation || s.text.len() < self.done_len {
                self.generation = s.generation; // the head was trimmed (or /clear): start over
                self.done_len = 0;
                self.done.clear();
            }
            let cut = s.text.rfind('\n').map_or(0, |i| i + 1);
            if cut > self.done_len {
                let new = split(&s.text[self.done_len..cut], self.done.last());
                self.done.extend(new);
                self.done_len = cut;
            }
            self.tail = if cut < s.text.len() { split(&s.text[cut..], self.done.last()) } else { vec![] };
        });
    }

    fn len(&self) -> usize {
        self.done.len() + self.tail.len()
    }

    fn get(&self, i: usize) -> Option<&ALine> {
        self.done.get(i).or_else(|| self.tail.get(i - self.done.len().min(i)))
    }

    fn plain(&self, i: usize) -> String {
        self.get(i).map(ansi::plain).unwrap_or_default()
    }
}

/// Fragments of one line with the chars from `a` up to `b` drawn reversed.
fn paint_sel(line: &ALine, sel: Option<(usize, usize)>) -> RLine<'static> {
    let Some((a, b)) = sel else { return to_rline(line) };
    let mut out = vec![];
    let mut pos = 0;
    for (style, text) in line {
        let chars: Vec<char> = text.chars().collect();
        let end = pos + chars.len();
        if end <= a || pos >= b {
            out.push(Span::styled(text.clone(), *style));
        } else {
            let (i, j) = (a.max(pos) - pos, b.min(end) - pos);
            if i > 0 {
                out.push(Span::styled(chars[..i].iter().collect::<String>(), *style));
            }
            out.push(Span::styled(chars[i..j].iter().collect::<String>(), style.add_modifier(Modifier::REVERSED)));
            if j < chars.len() {
                out.push(Span::styled(chars[j..].iter().collect::<String>(), *style));
            }
        }
        pos = end;
    }
    RLine::from(out)
}

/// What a keypress asks the app to do. Editing keys are handled inside and return `None`.
pub enum Action {
    None,
    Submit(String),
    /// Enter while the agent asked for a typed answer.
    Answer(String),
    Pick(usize),
    /// Ctrl+C with nothing to clear.
    Interrupt,
    Eof,
    /// Shift+Tab: next approval mode.
    Cycle,
}

pub struct Screen {
    tui: Arc<Tui>,
    title: Arc<dyn Fn() -> String + Send + Sync>,
    footer: Arc<dyn Fn() -> String + Send + Sync>,
    comp: Arc<Completions>,
    pub editor: Editor,
    tr: Transcript,
    /// First visible line while scrolled back; None follows new output.
    top: Option<usize>,
    /// (line, column) pairs, anchor then head.
    sel: Option<((usize, usize), (usize, usize))>,
    dragging: bool,
    /// While dragging: 1 = pointer at/above the top (scroll back), -1 = at/below the bottom, 0 = inside.
    edge: i32,
    click: (Instant, (usize, usize), u32),
    last_move: Instant,
    copied_at: Option<Instant>,
    /// F2 turns mouse capture off so the terminal's own selection works.
    pub mouse_on: bool,
    side_seen: bool,
    pub question: Option<(String, Vec<String>)>,
    pub line_mode: bool,
    main_rect: Rect,
    main_rows: usize,
    footer_cache: (Instant, String),
    input_top: usize,
}

impl Screen {
    pub fn new(tui: Arc<Tui>, title: Arc<dyn Fn() -> String + Send + Sync>, footer: Arc<dyn Fn() -> String + Send + Sync>, comp: Arc<Completions>, history: Option<std::path::PathBuf>) -> Screen {
        Screen {
            tui,
            title,
            footer,
            comp,
            editor: Editor::new(history),
            tr: Transcript::default(),
            top: None,
            sel: None,
            dragging: false,
            edge: 0,
            click: (Instant::now(), (0, 0), 0),
            last_move: Instant::now(),
            copied_at: None,
            mouse_on: true,
            side_seen: false,
            question: None,
            line_mode: false,
            main_rect: Rect::default(),
            main_rows: 20,
            footer_cache: (Instant::now() - Duration::from_secs(9), String::new()),
            input_top: 0,
        }
    }

    /// /clear: wipe the transcript and everything cached from it.
    pub fn clear(&mut self) {
        sink::clear();
        self.tr = Transcript::default();
        self.sel = None;
        self.top = None;
        self.dragging = false;
        self.edge = 0;
    }

    fn side_on(&mut self, cols: usize) -> bool {
        self.side_seen = self.side_seen || self.tui.side_has().iter().any(|b| *b);
        cols >= MIN_COLUMNS && self.side_seen
    }

    fn main_width(&mut self, cols: usize) -> usize {
        let side = if self.side_on(cols) { SIDE_WIDTH + 1 } else { 0 };
        cols.saturating_sub(MARGIN + side + 1).max(40)
    }

    // -- scrolling, selecting, copying -----------------------------------------------------------------------------
    /// Positive goes back in time. Scrolling past the newest line goes back to following.
    pub fn scroll(&mut self, delta: isize) {
        let max_top = self.tr.len().saturating_sub(self.main_rows.max(1));
        let cur = self.top.unwrap_or(max_top).min(max_top) as isize;
        let t = (cur - delta).clamp(0, max_top as isize) as usize;
        self.top = if t >= max_top { None } else { Some(t) };
    }

    fn first_line(&self) -> usize {
        let max_top = self.tr.len().saturating_sub(self.main_rows.max(1));
        self.top.map_or(max_top, |t| t.min(max_top))
    }

    /// Columns of line i that hold the text itself, without the rail glyph on the left (" ▎ ", " ◆ ", " │ ") or a
    /// panel's side border on the right. Copying skips the rest.
    fn body(&self, i: usize) -> (usize, usize) {
        let line: Vec<char> = self.tr.plain(i).chars().collect();
        let mut lo = 0;
        if line.len() >= 2 && line[0] == ' ' && "▎◆│▍".contains(line[1]) {
            lo = if line.get(2) == Some(&' ') { 3 } else { 2 };
        } else if line.len() >= 2 && line[0] == '▌' && line[1] == ' ' {
            lo = 2;
        } else if !line.is_empty() && "│┃╭╰".contains(line[0]) {
            lo = if line.get(1).map_or(false, |c| c.is_whitespace()) { 2 } else { 1 };
        }
        let mut hi = line.iter().rposition(|c| !c.is_whitespace()).map_or(0, |p| p + 1);
        if hi > lo && "│┃".contains(line[hi - 1]) {
            hi = line[..hi - 1].iter().rposition(|c| !c.is_whitespace()).map_or(0, |p| p + 1);
        }
        (lo, hi.max(lo))
    }

    /// The whole transcript as plain text, for /copy all.
    pub fn all_text(&mut self) -> String {
        self.tr.sync();
        (0..self.tr.len()).map(|i| {
            let (lo, hi) = self.body(i);
            self.tr.plain(i).chars().skip(lo).take(hi - lo).collect::<String>()
        }).collect::<Vec<_>>().join("\n").trim().to_string()
    }

    fn span(&self, i: usize, a: (usize, usize), b: (usize, usize)) -> (usize, usize) {
        let n = self.tr.plain(i).chars().count();
        (if i == a.0 { a.1 } else { 0 }, if i == b.0 { n.min(b.1 + 1) } else { n })
    }

    fn selected_text(&self) -> String {
        let Some((x, y)) = self.sel else { return String::new() };
        let (a, b) = if x <= y { (x, y) } else { (y, x) };
        let mut rows = vec![];
        for i in a.0..=b.0.min(self.tr.len().saturating_sub(1)) {
            let (lo, hi) = self.span(i, a, b);
            let (top, end) = self.body(i);
            let (s, e) = (lo.max(top), hi.min(end));
            rows.push(self.tr.plain(i).chars().skip(s).take(e.saturating_sub(s)).collect::<String>().trim_end().to_string());
        }
        rows.join("\n").trim_matches('\n').to_string()
    }

    fn copy(&mut self) {
        let text = self.selected_text();
        if !text.is_empty() {
            copy_to_clipboard(&text);
            self.copied_at = Some(Instant::now());
        }
    }

    fn pos_at(&self, col: u16, row: u16) -> ((usize, usize), i32) {
        let r = self.main_rect;
        let rel = row as i32 - r.y as i32;
        let h = self.main_rows as i32;
        let edge = if rel <= 0 { 1 } else if rel >= h - 1 { -1 } else { 0 };
        let line = (self.first_line() as i32 + rel.clamp(0, h - 1)) as usize;
        ((line.min(self.tr.len().saturating_sub(1)), (col as i32 - r.x as i32).max(0) as usize), edge)
    }

    /// Drag to select, double-click for a word, triple-click for a line. Copies on release.
    pub fn mouse(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::ScrollUp => self.scroll(3),
            MouseEventKind::ScrollDown => self.scroll(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                let r = self.main_rect;
                if ev.column < r.x || ev.column >= r.x + r.width || ev.row < r.y || ev.row >= r.y + r.height {
                    return;
                }
                let (pos, _) = self.pos_at(ev.column, ev.row);
                let (when, where_, n) = self.click;
                let n = if when.elapsed().as_secs_f64() < 0.45 && where_ == pos { n + 1 } else { 1 };
                self.click = (Instant::now(), pos, n);
                let line: Vec<char> = self.tr.plain(pos.0).chars().collect();
                if n == 2 {
                    let (mut lo, mut hi) = (pos.1, pos.1);
                    while lo > 0 && lo - 1 < line.len() && !line[lo - 1].is_whitespace() {
                        lo -= 1;
                    }
                    while hi + 1 < line.len() && !line[hi + 1].is_whitespace() {
                        hi += 1;
                    }
                    self.sel = Some(((pos.0, lo), (pos.0, hi)));
                } else if n >= 3 {
                    let end = line.iter().rposition(|c| !c.is_whitespace()).unwrap_or(0);
                    self.sel = Some(((pos.0, 0), (pos.0, end)));
                } else {
                    self.sel = Some((pos, pos));
                }
                self.dragging = n == 1;
                if n > 1 {
                    self.copy();
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                let (pos, edge) = self.pos_at(ev.column, ev.row);
                if let Some(s) = &mut self.sel {
                    s.1 = pos;
                }
                self.edge = edge;
                self.last_move = Instant::now();
            }
            MouseEventKind::Up(MouseButton::Left) if self.dragging => {
                let (pos, _) = self.pos_at(ev.column, ev.row);
                self.dragging = false;
                self.edge = 0;
                match self.sel {
                    Some((a, b)) if a == b => self.sel = None, // a plain click selects nothing
                    Some((a, _)) => {
                        self.sel = Some((a, pos));
                        self.copy();
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    // -- keys ----------------------------------------------------------------------------------------------------------
    pub fn paste(&mut self, text: &str) {
        self.editor.insert_str(text);
        self.editor.refresh_menu(&self.comp);
    }

    pub fn key(&mut self, k: KeyEvent) -> Action {
        use KeyCode::*;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        if let Some((_, opts)) = &self.question {
            // single-key answers while an approval question is open (they win over typing)
            let n = opts.len();
            return match k.code {
                Char('c') if ctrl => Action::Pick(n - 1),
                Char(c) if c.is_ascii_digit() && c != '0' && (c as usize - '1' as usize) < n => Action::Pick(c as usize - '1' as usize),
                Char('y') | Char('Y') | Enter => Action::Pick(0),
                Char('n') | Char('N') | Esc => Action::Pick(n - 1),
                _ => Action::None, // swallow stray typing while a question is open
            };
        }
        match k.code {
            PageUp => self.scroll(self.main_rows as isize - 2),
            PageDown => self.scroll(-(self.main_rows as isize - 2)),
            Home if ctrl => self.top = Some(0),
            End if ctrl => self.top = None,
            F(2) => {
                self.mouse_on = !self.mouse_on;
                self.sel = None;
                self.dragging = false;
            }
            Esc => {
                if self.sel.is_some() {
                    self.sel = None;
                } else {
                    self.editor.menu = None;
                }
            }
            Enter if shift || alt => self.edit(|e| e.insert('\n')),
            Char('j') if ctrl => self.edit(|e| e.insert('\n')),
            Enter => {
                if self.editor.menu.as_ref().map_or(false, |m| m.sel.is_some()) {
                    self.editor.apply_menu(); // a highlighted suggestion: take it instead of submitting
                    self.editor.refresh_menu(&self.comp);
                    return Action::None;
                }
                if self.line_mode {
                    let t = self.editor.take();
                    self.line_mode = false;
                    return Action::Answer(t.trim().to_string());
                }
                if !self.editor.text().trim().is_empty() {
                    return Action::Submit(self.editor.take());
                }
            }
            Tab => {
                if self.editor.menu.is_none() {
                    self.editor.refresh_menu(&self.comp);
                }
                self.editor.menu_next(false);
            }
            BackTab => {
                if self.editor.menu.is_some() {
                    self.editor.menu_next(true);
                } else {
                    return Action::Cycle;
                }
            }
            Up => {
                if self.editor.menu.is_some() { self.editor.menu_next(true) } else { self.editor.up() }
            }
            Down => {
                if self.editor.menu.is_some() { self.editor.menu_next(false) } else { self.editor.down() }
            }
            Left => self.editor.left(),
            Right => self.editor.right(),
            Home => self.editor.home(),
            End => self.editor.end(),
            Char('a') if ctrl => self.editor.home(),
            Char('e') if ctrl => self.editor.end(),
            Char('k') if ctrl => self.edit(|e| e.kill_to_end()),
            Char('u') if ctrl => self.edit(|e| e.kill_to_start()),
            Char('w') if ctrl => self.edit(|e| e.kill_word()),
            Char('c') if ctrl => {
                if !self.editor.is_empty() {
                    self.editor.reset();
                } else {
                    return Action::Interrupt;
                }
            }
            Char('d') if ctrl => {
                if self.editor.is_empty() { return Action::Eof } else { self.edit(|e| e.delete()) }
            }
            Backspace => self.edit(|e| e.backspace()),
            Delete => self.edit(|e| e.delete()),
            Char(c) if !ctrl && !alt => self.edit(|e| e.insert(c)),
            _ => {}
        }
        Action::None
    }

    fn edit(&mut self, f: impl FnOnce(&mut Editor)) {
        f(&mut self.editor);
        self.editor.refresh_menu(&self.comp);
    }

    // -- redraw bookkeeping ----------------------------------------------------------------------------------------------
    /// The melon blinks when idle and widens its eyes while the agent is working.
    fn face(&self) -> &'static str {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        if self.tui.is_turn_active() {
            ["O.O", "o.o", "O.O", "o.O", "O.o"][((now * 3.0) as usize) % 5]
        } else if ((now * 2.0) as usize) % 9 == 0 {
            "-.-"
        } else {
            "o.o"
        }
    }

    /// True while something moves (a turn, a question, a drag, a running sub-agent): the clock then drives redraws.
    pub fn animating(&self) -> bool {
        self.tui.is_turn_active() || self.dragging || self.question.is_some() || !self.tui.snapshot_side().running.is_empty()
    }

    /// What would look different on the next redraw while idle: the blink, the title, a flash or a glow wearing off.
    pub fn idle_sig(&self) -> (String, String, bool, bool) {
        (self.face().to_string(), (self.title)(), self.copied_at.map_or(false, |t| t.elapsed().as_secs_f64() < 2.0), !self.tui.snapshot_side().fresh.is_empty())
    }

    fn status(&self, w: usize) -> Vec<RLine<'static>> {
        let mut v: Vec<RLine<'static>> = self.tui.status_lines().iter().flat_map(|s| ansi::parse(s)).map(|l| to_rline(&l)).collect();
        if let Some((q, opts)) = &self.question {
            let (head, context) = q.split_once('\n').unwrap_or((q, ""));
            v.push(RLine::from(vec![sp(bold_fg(LAVENDER), " ? "), sp(Style::default().add_modifier(Modifier::BOLD), head.to_string())]));
            if !context.is_empty() {
                v.push(RLine::from(sp(fg(MUTED), format!("   {context}"))));
                v.push(RLine::default());
            }
            for (i, opt) in opts.iter().enumerate() {
                let (label, why) = opt.split_once(" — ").unwrap_or((opt, ""));
                v.push(RLine::from(vec![
                    sp(bold_fg(if i + 1 == opts.len() { CORAL } else { ACCENT }), format!("   [{}] ", i + 1)),
                    sp(if why.is_empty() { fg(FG) } else { bold_fg(FG) }, label.to_string()),
                    sp(fg(MUTED), if i == 0 { "  ⏎" } else { "" }),
                ]));
                if !why.is_empty() {
                    v.push(RLine::from(sp(fg(MUTED), format!("       {why}"))));
                }
            }
        }
        if !v.is_empty() {
            v.push(scanner(w));
        }
        v
    }

    // -- drawing -------------------------------------------------------------------------------------------------------
    pub fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let (cols, rows) = (area.width as usize, area.height as usize);
        if cols < 20 || rows < 6 {
            return;
        }
        self.tr.sync();
        let side = self.side_on(cols);
        self.tui.set_side_panel(side);
        let main_w = self.main_width(cols);
        sink::set_width(main_w);

        // holding the pointer at an edge keeps scrolling and stretches the selection with it
        if self.dragging && self.edge != 0 && self.sel.is_some() {
            self.scroll(2 * self.edge as isize);
            let row = if self.edge > 0 { self.first_line() } else { self.first_line() + self.main_rows.saturating_sub(1) };
            if let Some(s) = &mut self.sel {
                s.1 = (row.min(self.tr.len().saturating_sub(1)), if self.edge > 0 { 0 } else { 1_000_000 });
            }
        } else if self.dragging && self.last_move.elapsed().as_secs_f64() > 0.8 && self.sel.map_or(false, |s| s.0 != s.1) {
            self.dragging = false; // the release went somewhere we never heard about: finish anyway
            self.copy();
        }

        let status = self.status(main_w);
        let tw = cols.saturating_sub(4).max(1);
        let (wrapped, cursor) = wrap(&self.editor, tw);
        let cap = (rows * 2 / 5).max(3);
        let in_h = wrapped.len().clamp(1, cap);
        let box_h = in_h + 2;
        let box_y = rows.saturating_sub(1 + box_h);
        let s_h = status.len().min(box_y.saturating_sub(5));
        let stat_y = box_y - s_h;
        let body_h = stat_y.saturating_sub(1).max(1);

        // header
        let copied = self.copied_at.map_or(false, |t| t.elapsed().as_secs_f64() < 2.0);
        let busy = self.tui.is_turn_active();
        let face = self.face();
        let title = (self.title)();
        let left = format!("  ({face})  melon  ·  {title}");
        let (right, right_style) = if copied {
            ("✓ copied to clipboard  ".to_string(), bold_fg(MINT))
        } else if !self.mouse_on {
            ("native select on (wheel off) · F2 to go back  ".to_string(), fg(FAINT))
        } else if self.top.is_some() {
            ("scrolled back · PageDown / ctrl+end to return  ".to_string(), fg(AMBER))
        } else {
            ("drag to copy · /copy all · F2 native select  ".to_string(), fg(FAINT))
        };
        let pad = " ".repeat(cols.saturating_sub(left.chars().count() + right.chars().count()).max(1));
        let header = RLine::from(vec![
            sp(bold_fg(MINT), format!("  ({face})")),
            sp(bold_fg(if busy { LAVENDER } else { FG }), "  melon"),
            sp(fg(MUTED), format!("  ·  {title}{pad}")),
            sp(right_style, right),
        ]);
        f.render_widget(Paragraph::new(header).style(Style::default().bg(hx(BG_BOX))), Rect::new(0, 0, area.width, 1));

        // transcript
        let main = Rect::new(MARGIN as u16, 1, (main_w.min(cols.saturating_sub(MARGIN))) as u16, body_h as u16);
        self.main_rect = main;
        self.main_rows = body_h;
        let first = self.first_line();
        let sel = self.sel.map(|(x, y)| if x <= y { (x, y) } else { (y, x) });
        let lines: Vec<RLine<'static>> = (first..self.tr.len().min(first + body_h)).filter_map(|i| {
            let l = self.tr.get(i)?;
            Some(match sel {
                Some((a, b)) if a.0 <= i && i <= b.0 => {
                    let (s, e) = self.span(i, a, b);
                    paint_sel(l, Some((s, e)))
                }
                _ => to_rline(l),
            })
        }).collect();
        f.render_widget(Paragraph::new(lines), main);

        // side panel
        if side {
            let x = (cols - SIDE_WIDTH - 1) as u16;
            let rule: Vec<RLine> = (0..body_h).map(|_| RLine::from(sp(fg(RULE), "│"))).collect();
            f.render_widget(Paragraph::new(rule), Rect::new(x, 1, 1, body_h as u16));
            self.draw_side(f, Rect::new(x + 1, 1, SIDE_WIDTH as u16, body_h as u16));
        }

        // status (spinner, sub-agents, a question) and the scanner light
        if s_h > 0 {
            let skip = status.len() - s_h;
            f.render_widget(Paragraph::new(status.into_iter().skip(skip).collect::<Vec<_>>()), Rect::new(MARGIN as u16, stat_y as u16, main_w.min(cols - MARGIN) as u16, s_h as u16));
        }

        // input box: blank row, the text with an accent bar down its left edge, blank row
        let box_style = Style::default().bg(hx(BG_BOX)).fg(hx(FG));
        f.render_widget(Paragraph::new("").style(box_style), Rect::new(0, box_y as u16, area.width, box_h as u16));
        if cursor.0 < self.input_top {
            self.input_top = cursor.0;
        } else if cursor.0 >= self.input_top + in_h {
            self.input_top = cursor.0 + 1 - in_h;
        }
        self.input_top = self.input_top.min(wrapped.len().saturating_sub(in_h));
        let in_lines: Vec<RLine> = wrapped.iter().enumerate().skip(self.input_top).take(in_h).map(|(i, t)| {
            let lead = if i == 0 { sp(bold_fg(ACCENT).bg(hx(BG_BOX)), " ✦ ") } else { sp(box_style, "   ") };
            RLine::from(vec![sp(fg(ACCENT).bg(hx(BG_BOX)), "▍"), lead, sp(box_style, t.clone())])
        }).collect();
        f.render_widget(Paragraph::new(in_lines), Rect::new(0, (box_y + 1) as u16, area.width, in_h as u16));
        f.set_cursor_position((4 + cursor.1 as u16, (box_y + 1 + cursor.0.saturating_sub(self.input_top)) as u16));

        // completion menu, above the input box
        if let Some(m) = &self.editor.menu {
            let n = m.cands.len().min(8);
            let start = m.sel.map_or(0, |s| (s + 1).saturating_sub(n));
            let w = m.cands.iter().map(|c| c.display.chars().count()).max().unwrap_or(10).min(cols.saturating_sub(6)) + 2;
            let r = Rect::new(3, box_y.saturating_sub(n) as u16, w as u16, n as u16);
            f.render_widget(Clear, r);
            let items: Vec<RLine> = m.cands.iter().enumerate().skip(start).take(n).map(|(i, c)| {
                let st = if m.sel == Some(i) { Style::default().bg(hx(ACCENT)).fg(hx(INK)).add_modifier(Modifier::BOLD) } else { box_style };
                RLine::from(sp(st, format!(" {:<w$}", c.display, w = w - 1)))
            }).collect();
            f.render_widget(Paragraph::new(items), r);
        }

        // footer: mode chip, model, context bar, then key hints when there is room
        if self.footer_cache.0.elapsed().as_millis() > 400 {
            self.footer_cache = (Instant::now(), (self.footer)());
        }
        f.render_widget(Paragraph::new(footer_line(&self.footer_cache.1, cols)).style(Style::default().bg(hx(BG_PANEL))), Rect::new(0, (rows - 1) as u16, area.width, 1));
    }

    fn draw_side(&mut self, f: &mut Frame, r: Rect) {
        let data = self.tui.snapshot_side();
        let has = [!(data.running.is_empty() && data.finished.is_empty()), !data.read.is_empty(), !data.written.is_empty(), !data.commands.is_empty()];
        let shown: Vec<usize> = (0..4).filter(|k| has[*k]).collect();
        f.render_widget(Paragraph::new("").style(Style::default().bg(hx(BG_PANEL))), r);
        if shown.is_empty() {
            return;
        }
        // sections share the height equally, with a rule between neighbours
        let total = r.height as usize;
        let rules = shown.len() - 1;
        let avail = total.saturating_sub(rules);
        let mut y = r.y;
        for (i, k) in shown.iter().enumerate() {
            let h = avail / shown.len() + usize::from(i < avail % shown.len());
            if i > 0 {
                f.render_widget(Paragraph::new(RLine::from(sp(fg(RULE).bg(hx(BG_PANEL)), "─".repeat(r.width as usize)))), Rect::new(r.x, y, r.width, 1));
                y += 1;
            }
            let lines = section(*k, h, &data);
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(hx(BG_PANEL))), Rect::new(r.x, y, r.width, h as u16));
            y += h as u16;
        }
    }
}

fn wrap(ed: &Editor, tw: usize) -> (Vec<String>, (usize, usize)) {
    let mut rows = vec![String::new()];
    let (mut w, mut cursor) = (0, (0, 0));
    for (i, c) in ed.chars.iter().enumerate() {
        if i == ed.cur {
            cursor = (rows.len() - 1, w);
        }
        if *c == '\n' {
            rows.push(String::new());
            w = 0;
            continue;
        }
        let cw = if ed.secret { 1 } else { UnicodeWidthChar::width(*c).unwrap_or(0) };
        if w + cw > tw {
            rows.push(String::new());
            w = 0;
            if i == ed.cur {
                cursor = (rows.len() - 1, 0);
            }
        }
        rows.last_mut().unwrap().push(if ed.secret { '•' } else { *c });
        w += cw;
    }
    if ed.cur == ed.chars.len() {
        if w >= tw {
            rows.push(String::new());
            w = 0;
        }
        cursor = (rows.len() - 1, w);
    }
    (rows, cursor)
}

/// A thin light that sweeps back and forth above the input while the agent works.
fn scanner(width: usize) -> RLine<'static> {
    let w = width.clamp(10, 120);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let pos = ((t * 2.2).sin() + 1.0) / 2.0 * (w - 1) as f64;
    RLine::from((0..w).map(|i| {
        let glow = (1.0 - (i as f64 - pos).abs() / 7.0).max(0.0);
        sp(fg(&mix(RULE, ACCENT, glow)), if glow > 0.05 { "━" } else { "─" })
    }).collect::<Vec<_>>())
}

/// Mode chip, model in accent, context bar tinted by how full it is, details after.
fn footer_line(text: &str, cols: usize) -> RLine<'static> {
    let mut spans = vec![sp(Style::default(), " ")];
    let mut used = 1;
    for (i, part) in text.split("  ·  ").enumerate() {
        if i > 0 {
            spans.push(sp(fg(RULE), "  ·  "));
            used += 5;
        }
        let pct = part.split('%').next().filter(|_| part.contains('%')).and_then(|p| p.rsplit(' ').next()).and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
        let (st, t) = if i == 0 {
            (chip_style(LAVENDER), format!(" {part} "))
        } else if i == 1 {
            (bold_fg(ACCENT), part.to_string())
        } else if part.starts_with("ctx") {
            (fg(if pct < 60 { MINT } else if pct < 85 { AMBER } else { CORAL }), part.to_string())
        } else if part.starts_with('$') {
            (fg(AMBER), part.to_string())
        } else {
            (fg(MUTED), part.to_string())
        };
        used += t.chars().count();
        spans.push(sp(st, t));
    }
    let hints = [("enter", "send"), ("shift+enter", "newline"), ("shift+tab", "mode"), ("/", "commands"), ("@", "files")];
    let hw: usize = hints.iter().map(|(k, v)| k.len() + v.len() + 6).sum();
    if cols > used + hw + 2 {
        spans.push(sp(Style::default(), " ".repeat(cols - used - hw)));
        for (k, v) in hints {
            spans.push(sp(fg(FG).bg(hx(BG_RAISED)), format!(" {k} ")));
            spans.push(sp(fg(MUTED), format!(" {v}  ")));
        }
    }
    RLine::from(spans)
}

/// One side-panel section: a chip with the count, then the newest items that fit.
fn section(kind: usize, h: usize, d: &SideData) -> Vec<RLine<'static>> {
    let width = SIDE_WIDTH - 3;
    let (title, color) = [("AGENTS", ACCENT), ("READ", SKY), ("WRITTEN", AMBER), ("COMMANDS", LAVENDER)][kind];
    let count = [d.running.len() + d.finished.len(), d.read.len(), d.written.len(), d.commands.len()][kind];
    let mut head = vec![sp(Style::default(), " "), sp(chip_style(color), format!(" {title} "))];
    if kind == 0 && !d.running.is_empty() {
        head.push(sp(bold_fg(ACCENT), format!(" {} running", d.running.len())));
    } else {
        head.push(sp(bold_fg(color), format!(" {count}")));
    }
    if kind == 2 {
        let (a, r) = d.written.iter().fold((0, 0), |(a, r), (_, v)| (a + v.0, r + v.1));
        head.extend([sp(Style::default(), "  "), sp(fg(MINT), format!("+{a}")), sp(fg(CORAL), format!(" −{r}"))]);
    }
    let mut out = vec![RLine::from(head)];
    let mut items: Vec<RLine<'static>> = vec![];
    let glow = |fresh: bool| if fresh { Style::default().bg(hx(BG_GLOW)) } else { Style::default() };
    match kind {
        0 => {
            for (agent, recent, stats) in &d.running {
                items.push(RLine::from(vec![sp(fg(ACCENT), " ◆ "), sp(bold_fg(FG), agent_title(agent)), sp(fg(MUTED), format!("  {stats}"))]));
                items.push(RLine::from(vec![sp(fg(MUTED), "   ╰ "), sp(fg(FG), clip(recent, width - 5))]));
            }
            for (agent, ok, stats) in &d.finished {
                items.push(RLine::from(vec![sp(fg(if *ok { MINT } else { CORAL }), if *ok { " ✓ " } else { " ✗ " }), sp(fg(FG), agent_title(agent)), sp(fg(MUTED), format!("  {stats}"))]));
            }
        }
        1 => {
            for (path, n) in &d.read {
                let fresh = d.fresh.contains(&format!("read:{path}"));
                let tail = if *n > 1 { format!(" ×{n}") } else { String::new() };
                let c = clip(path, width - 2 - tail.chars().count());
                let (head, name) = c.rsplit_once('/').map_or((String::new(), c.clone()), |(h, n)| (format!("{h}/"), n.to_string()));
                items.push(RLine::from(vec![
                    if fresh { sp(fg(ACCENT).patch(glow(true)), " ●") } else { sp(fg(MUTED), "  ") },
                    sp(fg(MUTED).patch(glow(fresh)), head),
                    sp(bold_fg(FG).patch(glow(fresh)), name),
                    sp(fg(MUTED), tail),
                ]));
            }
        }
        2 => {
            for (path, (adds, dels, new)) in &d.written {
                let fresh = d.fresh.contains(&format!("write:{path}"));
                let total = (adds + dels).max(1);
                let cells = (((total + 1) as f64).log2() * 1.2).round().clamp(1.0, 6.0) as usize; // bar length grows slowly with change size
                let a_c = if *dels > 0 { (cells as f64 * *adds as f64 / total as f64).round() as usize } else { cells };
                let (na, nd) = (if *adds > 0 { format!("+{adds}") } else { String::new() }, if *dels > 0 { format!(" −{dels}") } else { String::new() });
                let tag = if *new { " new" } else { "" };
                let c = clip(path, width.saturating_sub(2 + na.chars().count() + nd.chars().count() + cells + tag.len() + 2));
                let (head, name) = c.rsplit_once('/').map_or((String::new(), c.clone()), |(h, n)| (format!("{h}/"), n.to_string()));
                items.push(RLine::from(vec![
                    if fresh { sp(fg(ACCENT).patch(glow(true)), " ●") } else { sp(fg(MUTED), "  ") },
                    sp(fg(MUTED).patch(glow(fresh)), head),
                    sp(bold_fg(FG).patch(glow(fresh)), name),
                    sp(fg(AMBER), format!("{tag} ")),
                    sp(fg(MINT).patch(glow(fresh)), na),
                    sp(fg(CORAL).patch(glow(fresh)), nd),
                    sp(Style::default(), " "),
                    sp(fg(MINT).patch(glow(fresh)), "█".repeat(a_c.min(cells))),
                    sp(fg(CORAL).patch(glow(fresh)), "█".repeat(cells - a_c.min(cells))),
                ]));
            }
        }
        _ => {
            for (i, (cmd, ok)) in d.commands.iter().enumerate() {
                let fresh = d.fresh.contains(&format!("cmd:{i}"));
                items.push(RLine::from(vec![
                    sp(bold_fg(if *ok { MINT } else { CORAL }).patch(glow(fresh)), if *ok { " ✓ " } else { " ✗ " }),
                    sp(fg(FG).patch(glow(fresh)), clip(&cmd.replace('\n', " "), width - 3)),
                ]));
            }
        }
    }
    let rows = h.saturating_sub(if kind == 0 { 1 } else { 1 }).max(1);
    let hidden = items.len().saturating_sub(rows);
    if hidden > 0 {
        let keep = rows - 1;
        let drop = items.len() - keep;
        out.push(RLine::from(sp(fg(MUTED), format!("  ⋯ {drop} {}", if kind == 0 { "more" } else { "earlier" }))));
        out.extend(items.into_iter().skip(drop));
    } else {
        out.extend(items);
    }
    out
}

/// System clipboard through pbcopy / wl-copy / xclip when there is one, and always the terminal's own OSC 52 escape
/// too, which also works over ssh.
pub fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    for cmd in [vec!["pbcopy"], vec!["wl-copy"], vec!["xclip", "-selection", "clipboard"], vec!["clip"]] {
        if which::which(cmd[0]).is_err() {
            continue;
        }
        if let Ok(mut child) = std::process::Command::new(cmd[0]).args(&cmd[1..]).stdin(std::process::Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            break;
        }
    }
    use base64::Engine;
    let mut o = std::io::stdout().lock();
    let _ = write!(o, "\x1b]52;c;{}\x07", base64::engine::general_purpose::STANDARD.encode(text));
    let _ = o.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_runs_fold() {
        let l = split("a\n\n\n\nb\n", None);
        assert_eq!(l.iter().map(ansi::plain).collect::<Vec<_>>(), vec!["a", "", "b"]);
    }

    #[test]
    fn input_wraps_and_tracks_cursor() {
        let mut e = Editor::new(None);
        e.insert_str("abcdefgh\nxy");
        let (rows, cur) = wrap(&e, 5);
        assert_eq!(rows, vec!["abcde", "fgh", "xy"]);
        assert_eq!(cur, (2, 2));
    }

    #[test]
    fn selection_paints_reversed() {
        let line: ALine = vec![(Style::default(), "hello world".to_string())];
        let l = paint_sel(&line, Some((2, 5)));
        assert_eq!(l.spans.len(), 3);
        assert_eq!(l.spans[1].content, "llo");
    }
}
