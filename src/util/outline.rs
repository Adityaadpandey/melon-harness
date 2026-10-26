//! Symbols of a source file with line ranges, so the model can read one function instead of a 1,300-line file.
//! Python uses the indentation reader (exact ranges); other languages use per-line patterns and end a symbol where the
//! next one at the same or lower indent starts, which is right for the usual one-declaration-per-block code.
use crate::util::pyscan;
use crate::util::re::{lit, Re};
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub start: usize,
    pub end: usize,
    pub indent: usize,
    /// Comment-derived, ends where the next symbol begins.
    pub marker: bool,
}

struct Pats {
    js: Re,
    method: Re,
    go: Re,
    rs: Re,
    md: Re,
    jsx_note: Re,
    banner: Re,
}

fn pats() -> &'static Pats {
    static P: OnceLock<Pats> = OnceLock::new();
    P.get_or_init(|| Pats {
        js: lit(r"^(?P<i>\s*)(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?(?:(?P<k>function\*?|class|interface|type|enum)\s+(?P<n>[A-Za-z_$][\w$]*)|(?:const|let|var)\s+(?P<c>[A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s*)?(?:\(|function|[A-Za-z_$][\w$]*\s*=>|<))"),
        method: lit(r"^(?P<i>\s{2,4})(?:(?:public|private|protected|static|async|readonly|get|set)\s+)*(?P<n>[A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\([^;]*\)\s*(?::\s*[^={]+)?\{\s*$"),
        go: lit(r"^(?P<k>func|type)\s+(?:\([^)]*\)\s*)?(?P<n>\w+)"),
        rs: lit(r"^(?P<i>\s*)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?P<k>fn|struct|enum|trait|impl|mod)\s+(?P<n>\w+)"),
        md: lit(r"^(?P<h>#{1,4})\s+(?P<n>.+)"),
        jsx_note: lit(r"^(?P<i>\s*)\{/\*\s*(?P<n>[^*]{3,80}?)\s*\*/\}\s*$"),
        banner: lit(r"^(?P<i>\s*)//\s*[─\-=]{2,}\s*(?P<n>[^─\-=][^─=]{2,70}?)\s*[─\-=]*\s*$"),
    })
}

const NOT_METHODS: &[&str] = &["if", "for", "while", "switch", "catch", "return", "function", "constructor_"];

fn python(src: &str) -> Vec<Symbol> {
    pyscan::defs(src)
        .into_iter()
        .map(|d| Symbol { name: d.name, kind: if d.is_class { "class".into() } else { "def".into() }, start: d.start, end: d.end, indent: d.parents.len() * 4, marker: false })
        .collect()
}

/// Named groups of the first match, by name, via the regex crate directly.
fn named(re: &Re, line: &str, names: &[&str]) -> Option<Vec<Option<String>>> {
    match re {
        Re::Fast(r) => r.captures(line).map(|c| names.iter().map(|n| c.name(n).map(|m| m.as_str().to_string())).collect()),
        Re::Fancy(r) => r.captures(line).ok().flatten().map(|c| names.iter().map(|n| c.name(n).map(|m| m.as_str().to_string())).collect()),
    }
}

pub fn outline(path: &str, src: &str) -> Vec<Symbol> {
    let ext = path.rsplit_once('.').map(|x| x.1.to_lowercase()).unwrap_or_default();
    if ext == "py" {
        return python(src);
    }
    let p = pats();
    let lines: Vec<&str> = src.lines().collect();
    let mut syms: Vec<Symbol> = vec![];
    for (i, line) in lines.iter().enumerate() {
        let no = i + 1;
        let mk = |name: String, kind: String, indent: usize, marker: bool| Symbol { name, kind, start: no, end: no, indent, marker };
        match ext.as_str() {
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
                if let Some(m) = named(&p.js, line, &["i", "k", "n", "c"]) {
                    syms.push(mk(m[2].clone().or(m[3].clone()).unwrap_or_default(), m[1].clone().unwrap_or_else(|| "const".into()), m[0].as_ref().map_or(0, |s| s.len()), false));
                    continue;
                }
                if let Some(m) = named(&p.method, line, &["i", "n"]) {
                    let n = m[1].clone().unwrap_or_default();
                    if !NOT_METHODS.contains(&n.as_str()) {
                        syms.push(mk(n, "method".into(), m[0].as_ref().map_or(0, |s| s.len()), false));
                        continue;
                    }
                }
                if let Some(m) = named(&p.jsx_note, line, &["i", "n"]).or_else(|| named(&p.banner, line, &["i", "n"])) {
                    // markers inside huge components: the only map of where things render
                    syms.push(mk(m[1].clone().unwrap_or_default(), "section".into(), m[0].as_ref().map_or(0, |s| s.len()), true));
                }
            }
            "go" => {
                if let Some(m) = named(&p.go, line, &["k", "n"]) {
                    syms.push(mk(m[1].clone().unwrap_or_default(), m[0].clone().unwrap_or_default(), 0, false));
                }
            }
            "rs" => {
                if let Some(m) = named(&p.rs, line, &["i", "k", "n"]) {
                    syms.push(mk(m[2].clone().unwrap_or_default(), m[1].clone().unwrap_or_default(), m[0].as_ref().map_or(0, |s| s.len()), false));
                }
            }
            "md" | "mdx" => {
                if let Some(m) = named(&p.md, line, &["h", "n"]) {
                    let h = m[0].clone().unwrap_or_default();
                    syms.push(mk(m[1].clone().unwrap_or_default().trim().to_string(), format!("h{}", h.len()), h.len(), false));
                }
            }
            _ => {}
        }
    }
    for i in 0..syms.len() {
        let indent = syms[i].indent;
        let bound = syms[i + 1..].iter().find(|t| t.indent <= indent).map_or(lines.len() + 1, |t| t.start) - 1;
        let mut end = bound;
        if !syms[i].marker {
            end = close(&lines, syms[i].start, bound);
        }
        let start = syms[i].start;
        let mut e = end.max(start);
        while e > start && lines.get(e - 1).map_or(false, |l| l.trim().is_empty()) {
            e -= 1; // don't count trailing blank lines
        }
        syms[i].end = e;
    }
    syms
}

/// Line where the declaration's brackets balance out (approximate: braces in strings are not parsed).
fn close(lines: &[&str], start: usize, bound: usize) -> usize {
    let (mut bal, mut opened) = (0i32, false);
    for no in start..=bound.min(lines.len()) {
        let line = lines[no - 1];
        for ch in line.chars() {
            match ch {
                '{' | '(' | '[' => {
                    bal += 1;
                    opened = true;
                }
                '}' | ')' | ']' => bal -= 1,
                _ => {}
            }
        }
        if opened && bal <= 0 {
            return no;
        }
        if !opened && line.trim_end().ends_with(';') {
            return no;
        }
    }
    bound
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ts_symbols() {
        let src = "export function a() {\n  return 1;\n}\n\nexport const b = () => {\n  x();\n};\n";
        let s = outline("x.ts", src);
        assert_eq!((s[0].name.as_str(), s[0].start, s[0].end), ("a", 1, 3));
        assert_eq!((s[1].name.as_str(), s[1].start, s[1].end), ("b", 5, 7));
    }
}
