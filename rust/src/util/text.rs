/// Rough token count. The providers report exact usage after each call, so this only has to be
/// close enough for budgeting before the first reply (4 bytes a token, as the old fallback did).
pub fn estimate_tokens(text: &str) -> usize {
    (text.len() / 4).max(1)
}

/// Cut a string at a char boundary no later than `max` bytes.
pub fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// First `n` chars (not bytes) of a string, like Python's `s[:n]`.
pub fn head_chars(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Python's `str.split_whitespace` joined back with single spaces.
pub fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
