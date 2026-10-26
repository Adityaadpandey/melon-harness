//! Keep the screen to what the user needs. Models narrate between tool calls even when told to write one short line.
//! The full text stays in history and traces, so the model keeps its notes; the user sees a single line.
use crate::util::text::squash;

/// Text this long with no tool call yet is a real answer: stop holding it back and stream it.
pub const HOLD_CHARS: usize = 1500;
const LINE_CHARS: usize = 140;
/// A final answer past this (outside reviews) earns the model a reminder on the next request.
pub const LONG_ANSWER_WORDS: usize = 250;

/// First meaningful line of text that came before a tool call, trimmed to one short line.
/// Example: '- Working notes:\n- a.ts:3 ...' -> 'Working notes:'
pub fn interim_line(text: &str) -> String {
    let strip = |l: &str| squash(l.trim_matches(|c: char| " \t-*•#>".contains(c)));
    let first = text.lines().map(|l| (l, strip(l))).find(|(l, _)| !l.trim_matches(|c: char| " \t-*•#>".contains(c)).is_empty()).map(|(_, s)| s).unwrap_or_default();
    if first.chars().count() <= LINE_CHARS {
        return first;
    }
    let cut: String = first.chars().take(LINE_CHARS).collect();
    format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |x| x.0).trim_end_matches([',', ';', ':']))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_line() {
        assert_eq!(interim_line("- Working notes:\n- a.ts:3 ..."), "Working notes:");
        assert_eq!(interim_line(""), "");
        let long = "word ".repeat(60);
        assert!(interim_line(&long).ends_with('…'));
    }
}
