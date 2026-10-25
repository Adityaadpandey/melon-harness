//! A light Python reader: where functions and classes start and end, their parameters, and what a file imports.
//! Not a parser, but indentation-aware and string-aware, which is what outlines, signature checks and import graphs
//! need. (The Python harness used `ast`; this keeps the same answers on ordinary code.)
use crate::util::re::{lit, Re};
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct PyDef {
    pub name: String,
    pub is_class: bool,
    /// 1-based line of the first decorator (or the `def`/`class` line).
    pub start: usize,
    pub end: usize,
    pub indent: usize,
    /// The text between the signature's parentheses (functions only).
    pub params: String,
    /// Names of the enclosing classes, outermost first ("Cart" for Cart.add).
    pub parents: Vec<String>,
}

fn def_re() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"^(\s*)(?:async\s+)?(def|class)\s+(\w+)"))
}

/// For each line: true when it starts inside a triple-quoted string.
fn in_string_map(lines: &[&str]) -> Vec<bool> {
    let mut out = Vec::with_capacity(lines.len());
    let mut open: Option<&str> = None;
    for l in lines {
        out.push(open.is_some());
        let mut rest: &str = l;
        loop {
            match open {
                Some(q) => match rest.find(q) {
                    Some(i) => {
                        rest = &rest[i + 3..];
                        open = None;
                    }
                    None => break,
                },
                None => {
                    let a = rest.find("\"\"\"");
                    let b = rest.find("'''");
                    let (i, q) = match (a, b) {
                        (Some(a), Some(b)) if a < b => (a, "\"\"\""),
                        (Some(a), None) => (a, "\"\"\""),
                        (_, Some(b)) => (b, "'''"),
                        (None, None) => break,
                    };
                    // a # before the quotes comments them out
                    if rest[..i].contains('#') {
                        break;
                    }
                    rest = &rest[i + 3..];
                    open = Some(q);
                }
            }
        }
    }
    out
}

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

/// Every `def` and `class`, in source order, with decorators included in the range.
pub fn defs(src: &str) -> Vec<PyDef> {
    let lines: Vec<&str> = src.lines().collect();
    let strs = in_string_map(&lines);
    let mut out: Vec<PyDef> = vec![];
    let mut stack: Vec<(usize, String)> = vec![]; // (indent, class name)
    for (i, l) in lines.iter().enumerate() {
        if strs[i] {
            continue;
        }
        let Some(c) = def_re().captures(l) else { continue };
        let indent = c[1].as_ref().map_or(0, |s| s.len());
        let is_class = c[2].as_deref() == Some("class");
        let name = c[3].clone().unwrap_or_default();
        // the header runs until its parentheses balance and the line ends with ':'
        let (mut depth, mut j, mut header) = (0i32, i, String::new());
        loop {
            let t = lines[j];
            header.push_str(t);
            header.push('\n');
            for ch in t.chars() {
                match ch {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth -= 1,
                    _ => {}
                }
            }
            if depth <= 0 && (t.split('#').next().unwrap_or("").trim_end().ends_with(':') || j + 1 >= lines.len() || !t.contains('(') && depth <= 0) {
                break;
            }
            j += 1;
            if j >= lines.len() {
                j = lines.len() - 1;
                break;
            }
        }
        // body: until the next code line indented no deeper than the header
        let mut end = j + 1;
        let mut k = j + 1;
        while k < lines.len() {
            let t = lines[k];
            if !strs[k] && !t.trim().is_empty() && !t.trim_start().starts_with('#') && indent_of(t) <= indent {
                break;
            }
            if !t.trim().is_empty() {
                end = k + 1;
            }
            k += 1;
        }
        if end < j + 1 {
            end = j + 1;
        }
        // decorators directly above
        let mut start = i;
        while start > 0 && lines[start - 1].trim_start().starts_with('@') && indent_of(lines[start - 1]) == indent && !strs[start - 1] {
            start -= 1;
        }
        while stack.last().map_or(false, |(ind, _)| *ind >= indent) {
            stack.pop();
        }
        let params = if is_class { String::new() } else { between_parens(&header) };
        out.push(PyDef { name: name.clone(), is_class, start: start + 1, end, indent, params, parents: stack.iter().map(|s| s.1.clone()).collect() });
        if is_class {
            stack.push((indent, name));
        }
    }
    out
}

fn between_parens(header: &str) -> String {
    let Some(open) = header.find('(') else { return String::new() };
    let mut depth = 0;
    for (i, ch) in header[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return header[open + 1..open + i].to_string();
                }
            }
            _ => {}
        }
    }
    header[open + 1..].to_string()
}

#[derive(Debug, Default, Clone)]
pub struct Sig {
    /// positional-or-keyword and positional-only parameters, in order
    pub positional: Vec<String>,
    pub kwonly: Vec<String>,
    /// how many of the positional ones have defaults
    pub positional_defaults: usize,
    pub kwonly_required: Vec<String>,
    pub vararg: Option<String>,
    pub kwarg: Option<String>,
}

fn split_top(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut d) = (vec![], String::new(), 0i32);
    let mut quote: Option<char> = None;
    for ch in s.chars() {
        if let Some(q) = quote {
            cur.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                cur.push(ch);
            }
            '(' | '[' | '{' => {
                d += 1;
                cur.push(ch);
            }
            ')' | ']' | '}' => {
                d -= 1;
                cur.push(ch);
            }
            ',' if d == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

pub fn parse_sig(params: &str) -> Sig {
    let mut sig = Sig::default();
    let mut after_star = false;
    for p in split_top(params) {
        if p == "/" {
            continue;
        }
        if p == "*" {
            after_star = true;
            continue;
        }
        if let Some(rest) = p.strip_prefix("**") {
            sig.kwarg = Some(rest.split([':', '=']).next().unwrap_or("").trim().to_string());
            continue;
        }
        if let Some(rest) = p.strip_prefix('*') {
            sig.vararg = Some(rest.split([':', '=']).next().unwrap_or("").trim().to_string());
            after_star = true;
            continue;
        }
        let has_default = p.contains('=');
        let name = p.split([':', '=']).next().unwrap_or("").trim().to_string();
        if after_star {
            if !has_default {
                sig.kwonly_required.push(name.clone());
            }
            sig.kwonly.push(name);
        } else {
            if has_default {
                sig.positional_defaults += 1;
            }
            sig.positional.push(name);
        }
    }
    sig
}

#[derive(Debug, Clone)]
pub struct PyImport {
    /// Leading dots of a relative import (0 for absolute).
    pub level: usize,
    pub module: String,
    pub names: Vec<String>,
}

/// `import a.b, c` and `from x.y import a, b` (parenthesised lists included), anywhere in the file.
pub fn imports(src: &str) -> Vec<PyImport> {
    static IMP: OnceLock<Re> = OnceLock::new();
    static FROM: OnceLock<Re> = OnceLock::new();
    let imp = IMP.get_or_init(|| lit(r"^\s*import\s+([\w.,\s]+?)(?:\s+as\s+\w+)?\s*(?:#.*)?$"));
    let from = FROM.get_or_init(|| lit(r"^\s*from\s+(\.*)([\w.]*)\s+import\s+(.*)$"));
    let lines: Vec<&str> = src.lines().collect();
    let strs = in_string_map(&lines);
    let mut out = vec![];
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        i += 1;
        if strs[i - 1] {
            continue;
        }
        if let Some(c) = from.captures(l) {
            let level = c[1].as_ref().map_or(0, |s| s.len());
            let module = c[2].clone().unwrap_or_default();
            let mut tail = c[3].clone().unwrap_or_default();
            if tail.contains('(') && !tail.contains(')') {
                while i < lines.len() {
                    tail.push(' ');
                    tail.push_str(lines[i]);
                    i += 1;
                    if tail.contains(')') {
                        break;
                    }
                }
            }
            let tail = tail.split('#').next().unwrap_or("").replace(['(', ')'], "");
            let names = tail.split(',').filter_map(|n| n.trim().split_whitespace().next().map(String::from)).filter(|n| !n.is_empty() && n != "*").collect();
            out.push(PyImport { level, module, names });
        } else if let Some(c) = imp.captures(l) {
            for m in c[1].clone().unwrap_or_default().split(',') {
                let m = m.trim().split_whitespace().next().unwrap_or("");
                if !m.is_empty() {
                    out.push(PyImport { level: 0, module: m.to_string(), names: vec![] });
                }
            }
        }
    }
    out
}

/// Does the file hold a syntax error we can see without Python? (unbalanced brackets or an unterminated string)
pub fn looks_balanced(src: &str) -> bool {
    let (mut depth, mut quote): (i32, Option<char>) = (0, None);
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '#' => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
            }
            '"' | '\'' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    depth == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "import os, sys\nfrom a.b import (c,\n    d)\n\n@dec\nclass Cart:\n    \"\"\"def fake(self): pass\"\"\"\n    def add(self, item, qty=1, *, tag):\n        return 1\n\n    def _x(self):\n        pass\n\ndef top(a, b=2, *args, **kw):\n    pass\n";

    #[test]
    fn finds_defs() {
        let d = defs(SRC);
        let names: Vec<&str> = d.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["Cart", "add", "_x", "top"]);
        assert_eq!(d[0].start, 5); // decorator included
        assert_eq!(d[1].parents, vec!["Cart"]);
        assert_eq!((d[1].start, d[1].end), (8, 9));
        let s = parse_sig(&d[1].params);
        assert_eq!(s.positional, vec!["self", "item", "qty"]);
        assert_eq!(s.positional_defaults, 1);
        assert_eq!(s.kwonly_required, vec!["tag"]);
        let t = parse_sig(&d[3].params);
        assert_eq!((t.vararg.as_deref(), t.kwarg.as_deref()), (Some("args"), Some("kw")));
    }

    #[test]
    fn finds_imports() {
        let i = imports(SRC);
        assert_eq!(i[0].module, "os");
        assert_eq!(i[2].module, "a.b");
        assert_eq!(i[2].names, vec!["c", "d"]);
    }
}
