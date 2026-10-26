//! The multi-line input box's text: cursor, editing keys, history, and the completion menu state.
use super::input::Completions;
use rustyline::completion::Pair;
use std::path::PathBuf;

pub struct Menu {
    pub cands: Vec<Pair>,
    /// None until Tab/arrows pick one, so Enter still submits what was typed.
    pub sel: Option<usize>,
    /// Char index where the completed text starts.
    pub start: usize,
}

pub struct Editor {
    pub chars: Vec<char>,
    /// Cursor as a char index.
    pub cur: usize,
    history: Vec<String>,
    hist_pos: Option<usize>,
    draft: String,
    history_file: Option<PathBuf>,
    pub menu: Option<Menu>,
    /// Shown as dots (an API key being pasted).
    pub secret: bool,
}

impl Editor {
    pub fn new(history_file: Option<PathBuf>) -> Editor {
        let history = history_file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|t| t.lines().map(|l| l.replace("\\n", "\n").replace("\\\\", "\\")).collect())
            .unwrap_or_default();
        Editor { chars: vec![], cur: 0, history, hist_pos: None, draft: String::new(), history_file, menu: None, secret: false }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn set(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.cur = self.chars.len();
        self.menu = None;
    }

    pub fn reset(&mut self) {
        self.set("");
        self.hist_pos = None;
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cur, c);
        self.cur += 1;
        self.hist_pos = None;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars().filter(|c| *c != '\r') {
            self.insert(c);
        }
    }

    pub fn backspace(&mut self) {
        if self.cur > 0 {
            self.cur -= 1;
            self.chars.remove(self.cur);
        }
    }

    pub fn delete(&mut self) {
        if self.cur < self.chars.len() {
            self.chars.remove(self.cur);
        }
    }

    pub fn left(&mut self) {
        self.cur = self.cur.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cur = (self.cur + 1).min(self.chars.len());
    }

    fn line_start(&self, at: usize) -> usize {
        self.chars[..at].iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.chars[at..].iter().position(|c| *c == '\n').map_or(self.chars.len(), |i| at + i)
    }

    pub fn home(&mut self) {
        self.cur = self.line_start(self.cur);
    }

    pub fn end(&mut self) {
        self.cur = self.line_end(self.cur);
    }

    pub fn kill_to_end(&mut self) {
        let e = self.line_end(self.cur);
        self.chars.drain(self.cur..e);
    }

    pub fn kill_to_start(&mut self) {
        let s = self.line_start(self.cur);
        self.chars.drain(s..self.cur);
        self.cur = s;
    }

    pub fn kill_word(&mut self) {
        let mut s = self.cur;
        while s > 0 && self.chars[s - 1].is_whitespace() {
            s -= 1;
        }
        while s > 0 && !self.chars[s - 1].is_whitespace() {
            s -= 1;
        }
        self.chars.drain(s..self.cur);
        self.cur = s;
    }

    pub fn row(&self) -> usize {
        self.chars[..self.cur].iter().filter(|c| **c == '\n').count()
    }

    pub fn line_count(&self) -> usize {
        self.chars.iter().filter(|c| **c == '\n').count() + 1
    }

    /// Up: a line up in the text, or earlier history from the first line.
    pub fn up(&mut self) {
        if self.row() == 0 {
            self.history_back();
            return;
        }
        let col = self.cur - self.line_start(self.cur);
        let prev_end = self.line_start(self.cur) - 1;
        let prev_start = self.line_start(prev_end);
        self.cur = (prev_start + col).min(prev_end);
    }

    pub fn down(&mut self) {
        if self.row() + 1 >= self.line_count() {
            self.history_forward();
            return;
        }
        let col = self.cur - self.line_start(self.cur);
        let next_start = self.line_end(self.cur) + 1;
        let next_end = self.line_end(next_start);
        self.cur = (next_start + col).min(next_end);
    }

    fn history_back(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = match self.hist_pos {
            None => {
                self.draft = self.text();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.hist_pos = Some(pos);
        let h = self.history[pos].clone();
        self.set(&h);
    }

    fn history_forward(&mut self) {
        let Some(p) = self.hist_pos else { return };
        if p + 1 >= self.history.len() {
            self.hist_pos = None;
            let d = std::mem::take(&mut self.draft);
            self.set(&d);
        } else {
            self.hist_pos = Some(p + 1);
            let h = self.history[p + 1].clone();
            self.set(&h);
        }
    }

    /// The finished message; it goes into history (and the history file) unless it is empty or a repeat.
    pub fn take(&mut self) -> String {
        let t = self.text();
        if !t.trim().is_empty() && self.history.last() != Some(&t) && !self.secret {
            self.history.push(t.clone());
            if let Some(f) = &self.history_file {
                if let Some(d) = f.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                use std::io::Write;
                if let Ok(mut h) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
                    let _ = writeln!(h, "{}", t.replace('\\', "\\\\").replace('\n', "\\n"));
                }
            }
        }
        self.reset();
        t
    }

    // -- completion ---------------------------------------------------------------------------------------------
    /// Recompute the menu for the text before the cursor (it pops up while typing a `/` or `@`).
    pub fn refresh_menu(&mut self, comp: &Completions) {
        if self.secret {
            self.menu = None;
            return;
        }
        let before: String = self.chars[..self.cur].iter().collect();
        let wants = (before.starts_with('/') && !before.contains('\n')) || before.rsplit(char::is_whitespace).next().map_or(false, |w| w.starts_with('@'));
        if !wants {
            self.menu = None;
            return;
        }
        match comp.complete(&before, before.len()) {
            Ok((start_byte, cands)) if !cands.is_empty() => {
                let start = before[..start_byte].chars().count();
                let keep = self.menu.as_ref().and_then(|m| m.sel.filter(|_| m.start == start));
                self.menu = Some(Menu { sel: keep.filter(|s| *s < cands.len()), cands, start });
            }
            _ => self.menu = None,
        }
    }

    /// Tab / Down in the menu.
    pub fn menu_next(&mut self, back: bool) {
        if let Some(m) = &mut self.menu {
            let n = m.cands.len();
            m.sel = Some(match (m.sel, back) {
                (None, false) => 0,
                (None, true) => n - 1,
                (Some(s), false) => (s + 1) % n,
                (Some(s), true) => (s + n - 1) % n,
            });
        }
    }

    /// Put the highlighted candidate into the text. True when one was applied.
    pub fn apply_menu(&mut self) -> bool {
        let Some(m) = self.menu.take() else { return false };
        let Some(sel) = m.sel else {
            self.menu = Some(m);
            return false;
        };
        let rep: Vec<char> = m.cands[sel].replacement.chars().collect();
        self.chars.splice(m.start..self.cur, rep.iter().cloned());
        self.cur = m.start + rep.len();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_and_history() {
        let mut e = Editor::new(None);
        e.insert_str("hello world");
        e.kill_word();
        assert_eq!(e.text(), "hello ");
        e.insert_str("a\nbc");
        e.up();
        assert_eq!(e.row(), 0);
        e.reset();
        e.insert_str("one");
        e.take();
        e.insert_str("two");
        e.take();
        e.insert_str("draft");
        e.up();
        assert_eq!(e.text(), "two");
        e.up();
        assert_eq!(e.text(), "one");
        e.down();
        e.down();
        assert_eq!(e.text(), "draft");
    }

    #[test]
    fn line_moves() {
        let mut e = Editor::new(None);
        e.insert_str("ab\ncdef");
        e.up();
        assert_eq!((e.row(), e.cur), (0, 2));
        e.down();
        assert_eq!(e.row(), 1);
        e.home();
        assert_eq!(e.cur, 3);
        e.kill_to_end();
        assert_eq!(e.text(), "ab\n");
    }
}
