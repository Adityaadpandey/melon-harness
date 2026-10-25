//! One regex type for the whole program. The `regex` crate is fast but has no lookaround or
//! backreferences, which models use in grep patterns all the time; those fall back to `fancy-regex`.
use fancy_regex::Regex as Fancy;
use regex::Regex as Fast;

#[derive(Clone, Debug)]
pub enum Re {
    Fast(Fast),
    Fancy(Fancy),
}

impl Re {
    pub fn new(pattern: &str, ignore_case: bool) -> Result<Re, String> {
        let p = if ignore_case { format!("(?i){pattern}") } else { pattern.to_string() };
        match Fast::new(&p) {
            Ok(r) => Ok(Re::Fast(r)),
            Err(fast_err) => Fancy::new(&p).map(Re::Fancy).map_err(|e| {
                // the fast engine's message is usually the clearer one for plain syntax mistakes
                let msg = e.to_string();
                if msg.contains("look") || msg.contains("backref") { msg } else { fast_err.to_string() }
            }),
        }
    }

    /// A multi-line pattern, where `^` and `$` hold at every line.
    pub fn multiline(&self) -> Option<Re> {
        match self {
            Re::Fast(r) => Fast::new(&format!("(?m){}", r.as_str())).ok().map(Re::Fast),
            Re::Fancy(r) => Fancy::new(&format!("(?m){}", r.as_str())).ok().map(Re::Fancy),
        }
    }

    pub fn is_match(&self, text: &str) -> bool {
        match self {
            Re::Fast(r) => r.is_match(text),
            Re::Fancy(r) => r.is_match(text).unwrap_or(false),
        }
    }

    /// Replace every match (`$1` style group references work in both engines).
    pub fn replace_all(&self, text: &str, rep: &str) -> String {
        match self {
            Re::Fast(r) => r.replace_all(text, rep).into_owned(),
            Re::Fancy(r) => r.replace_all(text, rep).into_owned(),
        }
    }

    pub fn replace(&self, text: &str, rep: &str) -> String {
        match self {
            Re::Fast(r) => r.replace(text, rep).into_owned(),
            Re::Fancy(r) => r.replace(text, rep).into_owned(),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Re::Fast(r) => r.as_str(),
            Re::Fancy(r) => r.as_str(),
        }
    }

    /// Start and end of the first match.
    pub fn find(&self, text: &str) -> Option<(usize, usize)> {
        match self {
            Re::Fast(r) => r.find(text).map(|m| (m.start(), m.end())),
            Re::Fancy(r) => r.find(text).ok().flatten().map(|m| (m.start(), m.end())),
        }
    }

    /// Capture groups of the first match; group 0 is the whole match.
    pub fn captures(&self, text: &str) -> Option<Vec<Option<String>>> {
        match self {
            Re::Fast(r) => r.captures(text).map(|c| c.iter().map(|m| m.map(|m| m.as_str().to_string())).collect()),
            Re::Fancy(r) => r.captures(text).ok().flatten().map(|c| c.iter().map(|m| m.map(|m| m.as_str().to_string())).collect()),
        }
    }
}

/// Compile a pattern that is known to be valid (a literal in the source).
pub fn lit(pattern: &str) -> Re {
    Re::new(pattern, false).unwrap_or_else(|e| panic!("bad built-in pattern {pattern}: {e}"))
}

/// Same, case-insensitive.
pub fn lit_i(pattern: &str) -> Re {
    Re::new(pattern, true).unwrap_or_else(|e| panic!("bad built-in pattern {pattern}: {e}"))
}
