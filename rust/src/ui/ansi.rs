//! SGR escapes (what the renderers print) to styled spans the screen can draw.
use ratatui::style::{Color, Modifier, Style};

pub type Line = Vec<(Style, String)>;

fn apply(style: Style, params: &[u32]) -> Style {
    let mut s = style;
    let mut i = 0;
    while i < params.len() {
        match params[i] {
            0 => s = Style::default(),
            1 => s = s.add_modifier(Modifier::BOLD),
            2 => s = s.add_modifier(Modifier::DIM),
            3 => s = s.add_modifier(Modifier::ITALIC),
            4 => s = s.add_modifier(Modifier::UNDERLINED),
            7 => s = s.add_modifier(Modifier::REVERSED),
            9 => s = s.add_modifier(Modifier::CROSSED_OUT),
            22 => s = s.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => s = s.remove_modifier(Modifier::ITALIC),
            39 => s = s.fg(Color::Reset),
            49 => s = s.bg(Color::Reset),
            n @ (38 | 48) => {
                let color = match params.get(i + 1) {
                    Some(2) if i + 4 < params.len() => {
                        let c = Color::Rgb(params[i + 2] as u8, params[i + 3] as u8, params[i + 4] as u8);
                        i += 4;
                        Some(c)
                    }
                    Some(5) if i + 2 < params.len() => {
                        let c = Color::Indexed(params[i + 2] as u8);
                        i += 2;
                        Some(c)
                    }
                    _ => None,
                };
                if let Some(c) = color {
                    s = if n == 38 { s.fg(c) } else { s.bg(c) };
                }
            }
            n @ 30..=37 => s = s.fg(Color::Indexed((n - 30) as u8)),
            n @ 40..=47 => s = s.bg(Color::Indexed((n - 40) as u8)),
            n @ 90..=97 => s = s.fg(Color::Indexed((n - 90 + 8) as u8)),
            _ => {}
        }
        i += 1;
    }
    s
}

/// Split text into lines of styled spans. Style carries across line breaks, as a terminal would.
pub fn parse(text: &str) -> Vec<Line> {
    let mut lines: Vec<Line> = vec![vec![]];
    let mut style = Style::default();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let flush = |lines: &mut Vec<Line>, cur: &mut String, style: Style| {
        if !cur.is_empty() {
            lines.last_mut().unwrap().push((style, std::mem::take(cur)));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\x1b' && chars.get(i + 1) == Some(&'[') {
            let mut j = i + 2;
            let mut num = String::new();
            let mut params: Vec<u32> = vec![];
            while j < chars.len() && !chars[j].is_ascii_alphabetic() {
                if chars[j] == ';' {
                    params.push(num.parse().unwrap_or(0));
                    num.clear();
                } else {
                    num.push(chars[j]);
                }
                j += 1;
            }
            params.push(num.parse().unwrap_or(0));
            if chars.get(j) == Some(&'m') {
                flush(&mut lines, &mut cur, style);
                style = apply(style, &params);
            } // other CSI sequences (cursor moves, erases) mean nothing in a transcript
            i = j + 1;
            continue;
        }
        match c {
            '\n' => {
                flush(&mut lines, &mut cur, style);
                lines.push(vec![]);
            }
            '\r' | '\x07' => {}
            c => cur.push(c),
        }
        i += 1;
    }
    flush(&mut lines, &mut cur, style);
    lines
}

pub fn plain(line: &Line) -> String {
    line.iter().map(|(_, t)| t.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_colors_and_lines() {
        let l = parse("\x1b[1m\x1b[38;2;10;20;30mhi\x1b[0m there\nsecond");
        assert_eq!(l.len(), 2);
        assert_eq!(l[0][0].1, "hi");
        assert_eq!(l[0][0].0.fg, Some(Color::Rgb(10, 20, 30)));
        assert!(l[0][0].0.add_modifier.contains(Modifier::BOLD));
        assert_eq!(plain(&l[0]), "hi there");
        assert_eq!(l[0][1].0, Style::default());
        assert_eq!(plain(&l[1]), "second");
    }
}
