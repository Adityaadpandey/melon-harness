//! Colours and small text helpers for the terminal. Truecolor ANSI, the same palette the Python UI used: warm graphite
//! surfaces, one copper accent; green and red appear only where they carry meaning.
use unicode_width::UnicodeWidthStr;

pub const ACCENT: &str = "#e58a4b";
pub const LAVENDER: &str = "#cbb89a";
pub const SKY: &str = "#9db4b0";
pub const TEAL: &str = "#b0a690";
pub const MINT: &str = "#93b673";
pub const CORAL: &str = "#d96a56";
pub const AMBER: &str = "#d8b04e";
pub const FG: &str = "#e7e2d6";
pub const MUTED: &str = "#8b857a";
pub const INK: &str = "#1a1917";
pub const BG_BOX: &str = "#22201d";
pub const RULE: &str = "#3b3730";
pub const FAINT: &str = "#5a554c";
pub const ADD_BG: &str = "#1b2616";
pub const DEL_BG: &str = "#33181a";

pub const RESET: &str = "\x1b[0m";

fn rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(255);
    (p(0), p(2), p(4))
}

pub fn fg_code(hex: &str) -> String {
    let (r, g, b) = rgb(hex);
    format!("\x1b[38;2;{r};{g};{b}m")
}

pub fn bg_code(hex: &str) -> String {
    let (r, g, b) = rgb(hex);
    format!("\x1b[48;2;{r};{g};{b}m")
}

/// Whether to colour at all: a pipe, NO_COLOR or a dumb terminal gets plain text.
pub fn color_on() -> bool {
    use std::io::IsTerminal;
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() && std::env::var("TERM").map_or(true, |t| t != "dumb"))
}

pub fn paint(text: &str, hex: &str) -> String {
    if color_on() { format!("{}{text}{RESET}", fg_code(hex)) } else { text.to_string() }
}

pub fn bold(text: &str) -> String {
    if color_on() { format!("\x1b[1m{text}{RESET}") } else { text.to_string() }
}

pub fn italic(text: &str) -> String {
    if color_on() { format!("\x1b[3m{text}{RESET}") } else { text.to_string() }
}

pub fn bold_paint(text: &str, hex: &str) -> String {
    if color_on() { format!("\x1b[1m{}{text}{RESET}", fg_code(hex)) } else { text.to_string() }
}

pub fn on_bg(text: &str, hex: &str) -> String {
    if color_on() { format!("{}{text}{RESET}", bg_code(hex)) } else { text.to_string() }
}

/// A coloured chip: ` read ` in ink on the tool's colour.
pub fn chip(text: &str, hex: &str) -> String {
    if color_on() { format!("\x1b[1m{}{}{text}{RESET}", fg_code(INK), bg_code(hex)) } else { format!("[{}]", text.trim()) }
}

pub fn mix(a: &str, b: &str, t: f64) -> String {
    let (x, y) = (rgb(a), rgb(b));
    let m = |p: u8, q: u8| (p as f64 + (q as f64 - p as f64) * t).round() as u8;
    format!("#{:02x}{:02x}{:02x}", m(x.0, y.0), m(x.1, y.1), m(x.2, y.2))
}

/// Accent-to-sand wave that slides along the text, so a long wait still looks alive.
pub fn shimmer(text: &str, now: f64) -> String {
    if !color_on() {
        return text.to_string();
    }
    text.chars().enumerate().map(|(i, c)| format!("{}{c}", fg_code(&mix(ACCENT, LAVENDER, ((now * 4.0 - i as f64 * 0.45).sin() + 1.0) / 2.0)))).collect::<String>() + RESET
}

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub fn term_width() -> usize {
    crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(100).clamp(40, 200)
}

/// Strip ANSI escapes, for width maths.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for n in it.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

pub fn context_bar(pct: f64, w: usize) -> String {
    let filled = ((pct / 100.0 * w as f64).round() as i64).clamp(0, w as i64) as usize;
    format!("{}{} {:.0}%", "▰".repeat(filled), "▱".repeat(w - filled), pct)
}

pub fn clip_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helpers() {
        assert_eq!(strip_ansi("\x1b[1m\x1b[38;2;1;2;3mhi\x1b[0m"), "hi");
        assert_eq!(mix("#000000", "#ffffff", 0.5), "#808080");
        assert_eq!(context_bar(50.0, 10), "▰▰▰▰▰▱▱▱▱▱ 50%");
        assert_eq!(clip_chars("abcdef", 4), "abc…");
    }
}
