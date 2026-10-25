//! A small markdown renderer for assistant replies: headings, bullets, numbered lists, quotes, code fences, bold,
//! italics and inline code. Each line is word-wrapped first, then styled, and carries a rail down the left edge.
use super::style::*;
use crate::util::re::{lit, Re};
use std::sync::OnceLock;

fn inline_re() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"`([^`]+)`|\*\*([^*]+)\*\*|__([^_]+)__|\*([^*\s][^*]*)\*|(?:^|[^\w])_([^_\s][^_]*)_(?:[^\w]|$)"))
}

/// Inline spans of one line: `code`, **bold**, *italic*.
pub fn inline(line: &str) -> String {
    if !color_on() {
        return line.replace("**", "").replace('`', "");
    }
    let re = inline_re();
    let (mut out, mut at) = (String::new(), 0);
    while at < line.len() {
        let Some(c) = re.captures(&line[at..]) else { break };
        let (s, e) = re.find(&line[at..]).unwrap();
        out.push_str(&line[at..at + s]);
        let whole = &line[at + s..at + e];
        if let Some(code) = c.get(1).cloned().flatten() {
            out.push_str(&format!("{}{}{code}{RESET}", fg_code(AMBER), bg_code("#2a2723")));
        } else if let Some(b) = c.get(2).cloned().flatten().or(c.get(3).cloned().flatten()) {
            out.push_str(&bold_paint(&b, "#f6f1e4"));
        } else if let Some(i) = c.get(4).cloned().flatten() {
            out.push_str(&italic(&paint(&i, "#cfc8b8")));
        } else if let Some(i) = c.get(5).cloned().flatten() {
            // keep the non-word characters that bounded the match
            let lead = if whole.starts_with('_') { "" } else { &whole[..whole.find('_').unwrap_or(0)] };
            out.push_str(lead);
            out.push_str(&italic(&paint(&i, "#cfc8b8")));
            if whole.ends_with(|ch: char| !ch.is_alphanumeric() && ch != '_') {
                out.push(whole.chars().last().unwrap());
            }
        }
        at += e.max(1);
    }
    out.push_str(&line[at..]);
    out
}

fn wrap(text: &str, width: usize, indent: &str) -> Vec<String> {
    let opts = textwrap::Options::new(width.max(20)).subsequent_indent(indent).break_words(false);
    textwrap::wrap(text, opts).into_iter().map(|c| c.into_owned()).collect()
}

/// Rendered lines (without the rail) for a markdown block.
pub fn render_lines(md: &str, width: usize) -> Vec<String> {
    let heading = lit(r"^(#{1,4})\s+(.*)$");
    let bullet = lit(r"^(\s*)([-*+•])\s+(.*)$");
    let numbered = lit(r"^(\s*)(\d+[.)])\s+(.*)$");
    let mut out = vec![];
    let mut fence = false;
    for raw in md.trim_end().split('\n') {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            fence = !fence;
            if fence {
                let lang = line.trim_start().trim_start_matches('`').trim();
                if !lang.is_empty() {
                    out.push(paint(&format!("  {lang}"), FAINT));
                }
            }
            continue;
        }
        if fence {
            out.push(format!("  {}", on_bg(&format!(" {line} "), "#1e1c19").replace(" \x1b[0m", &format!(" {RESET}"))));
            continue;
        }
        if line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        if let Some(c) = heading.captures(line) {
            let level = c[1].as_ref().map_or(1, |h| h.len());
            let color = match level {
                1 => ACCENT,
                2 => LAVENDER,
                3 => SKY,
                _ => TEAL,
            };
            for l in wrap(&inline_plain(c[2].as_deref().unwrap_or("")), width, "  ") {
                out.push(format!("{}{}", paint("▌ ", color), bold_paint(&l, color)));
            }
            continue;
        }
        if line.trim() == "---" || line.trim() == "***" {
            out.push(paint(&"─".repeat(width.min(60)), RULE));
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix('>') {
            for l in wrap(rest.trim(), width.saturating_sub(2), "") {
                out.push(format!("{}{}", paint("▎ ", FAINT), italic(&paint(&inline(&l), "#a39c8c"))));
            }
            continue;
        }
        if line.trim_start().starts_with('|') {
            out.push(inline(line)); // tables stay as the model wrote them
            continue;
        }
        if let Some(c) = bullet.captures(line) {
            let pad = c[1].as_deref().unwrap_or("").to_string();
            for (i, l) in wrap(c[3].as_deref().unwrap_or(""), width.saturating_sub(pad.len() + 2), "  ").into_iter().enumerate() {
                out.push(format!("{pad}{}{}", if i == 0 { bold_paint("• ", ACCENT) } else { "  ".into() }, inline(&l)));
            }
            continue;
        }
        if let Some(c) = numbered.captures(line) {
            let pad = c[1].as_deref().unwrap_or("").to_string();
            let num = c[2].as_deref().unwrap_or("").to_string();
            let indent = " ".repeat(num.len() + 1);
            for (i, l) in wrap(c[3].as_deref().unwrap_or(""), width.saturating_sub(pad.len() + num.len() + 1), &indent).into_iter().enumerate() {
                out.push(format!("{pad}{}{}", if i == 0 { bold_paint(&format!("{num} "), ACCENT) } else { String::new() }, inline(&l)));
            }
            continue;
        }
        for l in wrap(line, width, "") {
            out.push(inline(&l));
        }
    }
    out
}

fn inline_plain(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}

/// A reply block with the rail down its left edge; the first block of a reply opens with a glyph.
pub fn render_block(md: &str, first: bool) -> String {
    let width = term_width().saturating_sub(4);
    let lines = render_lines(md, width);
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let glyph = if i == 0 && first { "◆" } else { "▎" };
            format!(" {} {l}", paint(glyph, LAVENDER))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// (finished markdown blocks, unfinished tail). A block ends at a blank line outside a code fence, unless the next line
/// continues a list, quote, table or indented block, so numbered lists and fenced code never get cut in half while text
/// is still streaming.
pub fn split_blocks(buf: &str) -> (String, String) {
    static CONT: OnceLock<Re> = OnceLock::new();
    let cont = CONT.get_or_init(|| lit(r"^(\s|[-*+]\s|\d+[.)]\s|>|\|)"));
    let lines: Vec<&str> = buf.split('\n').collect();
    let (mut in_fence, mut has_content, mut cut, mut pos) = (false, false, 0usize, 0usize);
    for i in 0..lines.len().saturating_sub(1) {
        let line = lines[i];
        pos += line.len() + 1;
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            has_content = true;
        } else if in_fence {
            continue;
        } else if !line.trim().is_empty() {
            has_content = true;
        } else if has_content {
            let last = lines[lines.len() - 1];
            let nxt = lines[i + 1..].iter().find(|l| !l.trim().is_empty() || **l == last);
            match nxt {
                None => continue, // nothing after the blank line yet: wait
                Some(n) if n.is_empty() => continue,
                Some(n) => {
                    if !cont.is_match(n) {
                        cut = pos;
                        has_content = false;
                    }
                }
            }
        }
    }
    (buf[..cut].to_string(), buf[cut..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_split_at_paragraphs_not_inside_lists_or_fences() {
        let (done, rest) = split_blocks("first para\n\nsecond");
        assert_eq!((done.as_str(), rest.as_str()), ("first para\n\n", "second"));
        let (done, _) = split_blocks("1. one\n\n2. two\n");
        assert_eq!(done, "");
        let (done, _) = split_blocks("```\ncode\n\nmore\n```\nafter");
        assert_eq!(done, "");
        let (done, rest) = split_blocks("a\n\nb\n\nc");
        assert_eq!((done.as_str(), rest.as_str()), ("a\n\nb\n\n", "c"));
    }

    #[test]
    fn renders_plain_without_color() {
        let lines = render_lines("# Title\n\n- one **bold**\n1. two\n\n```py\nx = 1\n```", 60);
        assert!(lines.iter().any(|l| strip_ansi(l).contains("Title")));
        assert!(lines.iter().any(|l| strip_ansi(l).contains("• one")));
        assert!(lines.iter().any(|l| strip_ansi(l).contains("x = 1")));
    }
}
