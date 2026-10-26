//! Pure edit planning: given file text and requested replacements, produce the new text or a precise error.
//! Matching is exact first. If that fails, a second pass ignores trailing whitespace and typographic quotes/dashes
//! (models often normalise those) and applies only when it matches exactly one place. CRLF line endings are kept.
use crate::util::re::{lit, Re};
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

fn numbered() -> &'static Re {
    // "12|code" as read_file shows it (or "12\tcode", "12→code" from other tools): the number is not in the file
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"^\s*\d+(\||\t|→|│|:\s)"))
}

/// old_string with read_file's line-number prefixes removed, or None when not every line carries one.
fn strip_numbers(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let body: Vec<&&str> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    if body.is_empty() || !body.iter().all(|l| numbered().is_match(l)) {
        return None;
    }
    Some(
        lines
            .iter()
            .map(|l| match numbered().find(l) {
                Some((_, e)) => l[e..].to_string(),
                None => l.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn norm(line: &str) -> String {
    let s: String = line.nfkc().collect();
    let s: String = s
        .chars()
        .map(|c| match c {
            '‘' | '’' => '\'',
            '“' | '”' => '"',
            '–' | '—' => '-',
            '\u{a0}' => ' ',
            c => c,
        })
        .collect();
    s.trim_end().to_string()
}

fn line_of(text: &str, index: usize) -> usize {
    text[..index].matches('\n').count() + 1
}

fn find_all(text: &str, needle: &str) -> Vec<usize> {
    let mut out = vec![];
    let mut from = 0;
    while let Some(i) = text[from..].find(needle) {
        out.push(from + i);
        from += i + needle.len();
    }
    out
}

fn fuzzy_span(text: &str, old: &str) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let want: Vec<String> = old.trim_matches('\n').split('\n').map(norm).collect();
    if !want.iter().any(|w| !w.is_empty()) {
        return vec![];
    }
    let mut starts = vec![0usize];
    for ln in &lines[..lines.len() - 1] {
        starts.push(starts.last().unwrap() + ln.len() + 1);
    }
    let mut spans = vec![];
    if lines.len() < want.len() {
        return spans;
    }
    for i in 0..=(lines.len() - want.len()) {
        if (0..want.len()).all(|k| norm(lines[i + k]) == want[k]) {
            let end_line = i + want.len() - 1;
            let mut end = starts[end_line] + lines[end_line].len();
            if old.ends_with('\n') && text[end..].starts_with('\n') {
                end += 1;
            }
            spans.push((starts[i], end));
        }
    }
    spans
}

fn hint(text: &str, old: &str) -> String {
    let first = old.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut near: Vec<(usize, &str)> =
        lines.iter().enumerate().filter(|(_, l)| !first.is_empty() && l.trim().contains(first)).map(|(i, l)| (i + 1, *l)).take(3).collect();
    if near.is_empty() {
        let mut scored: Vec<(f32, usize, &str)> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| (similar::TextDiff::from_chars(first, l.trim()).ratio(), i + 1, *l))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        near = scored.into_iter().take(2).filter(|(r, _, _)| *r > 0.6).map(|(_, i, l)| (i, l)).collect();
    }
    if !near.is_empty() {
        let rows: Vec<String> = near.iter().map(|(i, l)| format!("  {i}: {}", crate::util::text::head_chars(l.trim(), 100))).collect();
        format!("\nClosest lines:\n{}\nRe-read the file and copy the text exactly.", rows.join("\n"))
    } else {
        "\nRe-read the file with read_file and copy the text exactly (whitespace and indentation matter).".into()
    }
}

/// (new content, replacements) or an error message.
pub fn plan_edits(content: &str, edits: &[(String, String)], replace_all: bool) -> Result<(String, usize), String> {
    let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let bom = if content.starts_with('\u{feff}') { "\u{feff}" } else { "" };
    let text = content.strip_prefix('\u{feff}').unwrap_or(content).replace("\r\n", "\n");
    let mut spans: Vec<(usize, usize, String)> = vec![];

    for (n, (old, new)) in edits.iter().enumerate() {
        let label = if edits.len() > 1 { format!("edit {}: ", n + 1) } else { String::new() };
        let mut old = old.replace("\r\n", "\n");
        let mut new = new.replace("\r\n", "\n");
        if old.is_empty() {
            return Err(format!("{label}old_string is empty; use write_file to create or replace a file."));
        }
        let mut hits: Vec<(usize, usize)> = find_all(&text, &old).into_iter().map(|i| (i, i + old.len())).collect();
        if hits.is_empty() {
            if let Some(bare) = strip_numbers(&old) {
                // copied with the line numbers from read_file: drop them, and from new_string too when it carries them
                new = strip_numbers(&new).unwrap_or(new);
                old = bare;
                hits = find_all(&text, &old).into_iter().map(|i| (i, i + old.len())).collect();
            }
        }
        if hits.is_empty() {
            hits = fuzzy_span(&text, &old);
            if hits.len() > 1 {
                let w: Vec<String> = hits.iter().take(5).map(|(s, _)| line_of(&text, *s).to_string()).collect();
                return Err(format!(
                    "{label}text matches {} places ignoring whitespace/quote differences (lines {}). Add surrounding lines to make it unique.",
                    hits.len(),
                    w.join(", ")
                ));
            }
        }
        if hits.is_empty() {
            return Err(format!("{label}old_string not found.{}", hint(&text, &old)));
        }
        if hits.len() > 1 && !replace_all {
            let w: Vec<String> = hits.iter().take(6).map(|(s, _)| line_of(&text, *s).to_string()).collect();
            return Err(format!(
                "{label}old_string found {} times (lines {}). Add more context to make it unique, or set replace_all=true.",
                hits.len(),
                w.join(", ")
            ));
        }
        let take = if replace_all { hits.len() } else { 1 };
        for (s, e) in hits.into_iter().take(take) {
            spans.push((s, e, new.clone()));
        }
    }

    spans.sort_by_key(|s| (s.0, s.1));
    for w in spans.windows(2) {
        if w[1].0 < w[0].1 {
            return Err("Edits overlap. Merge changes that touch the same or adjacent lines into one edit.".into());
        }
    }
    let mut out = text.clone();
    for (s, e, new) in spans.iter().rev() {
        out.replace_range(*s..*e, new);
    }
    if out == text {
        return Err("No change: old_string equals new_string.".into());
    }
    if eol == "\r\n" {
        out = out.replace('\n', "\r\n");
    }
    Ok((format!("{bom}{out}"), spans.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(o: &str, n: &str) -> Vec<(String, String)> {
        vec![(o.to_string(), n.to_string())]
    }

    #[test]
    fn exact_and_unique() {
        let (out, n) = plan_edits("a\nb\nc\n", &p("b", "B"), false).unwrap();
        assert_eq!((out.as_str(), n), ("a\nB\nc\n", 1));
        assert!(plan_edits("x x", &p("x", "y"), false).unwrap_err().contains("2 times"));
        assert_eq!(plan_edits("x x", &p("x", "y"), true).unwrap().0, "y y");
    }

    #[test]
    fn crlf_and_numbers_and_quotes() {
        assert_eq!(plan_edits("a\r\nb\r\n", &p("b", "c"), false).unwrap().0, "a\r\nc\r\n");
        assert_eq!(plan_edits("x = 1\ny = 2\n", &p("1|x = 1\n2|y = 2", "x = 3\ny = 4"), false).unwrap().0, "x = 3\ny = 4\n");
        assert_eq!(plan_edits("say “hi”  \n", &p("say \"hi\"", "ok"), false).unwrap().0, "ok\n");
    }
}
