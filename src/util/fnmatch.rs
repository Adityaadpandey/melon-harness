//! Python's `fnmatch` semantics: `*` matches anything (slashes included), `?` one char, `[seq]` a set.
use crate::util::re::Re;

pub fn translate(pat: &str) -> String {
    let chars: Vec<char> = pat.chars().collect();
    let mut out = String::from("(?s)^");
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '[' => {
                let mut j = i;
                if j < chars.len() && chars[j] == '!' {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    j += 1;
                }
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                if j >= chars.len() {
                    out.push_str("\\[");
                } else {
                    let mut stuff: String = chars[i..j].iter().collect::<String>().replace('\\', "\\\\");
                    if let Some(rest) = stuff.strip_prefix('!') {
                        stuff = format!("^{rest}");
                    } else if stuff.starts_with('^') {
                        stuff = format!("\\{stuff}");
                    }
                    out.push('[');
                    out.push_str(&stuff);
                    out.push(']');
                    i = j + 1;
                }
            }
            c => out.push_str(&regex::escape(&c.to_string())),
        }
    }
    out.push('$');
    out
}

pub fn fnmatch(name: &str, pat: &str) -> bool {
    Re::new(&translate(pat), false).map(|r| r.is_match(name)).unwrap_or(false)
}

/// A pattern compiled once, for matching many names.
pub struct Matcher(Option<Re>);

impl Matcher {
    pub fn new(pat: &str) -> Matcher {
        Matcher(Re::new(&translate(pat), false).ok())
    }
    pub fn is_match(&self, name: &str) -> bool {
        self.0.as_ref().map_or(false, |r| r.is_match(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basics() {
        assert!(fnmatch("a/b.py", "*.py"));
        assert!(fnmatch(".env.local", ".env.*"));
        assert!(!fnmatch("env", ".env"));
        assert!(fnmatch("id_rsa.pub", "id_rsa*"));
        assert!(fnmatch("a1", "a[0-9]"));
    }
}
