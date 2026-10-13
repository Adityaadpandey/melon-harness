"""Symbols of a source file with line ranges, so the model can read one function
instead of a 1,300-line file. Python uses `ast` (exact ranges); other languages
use per-line patterns and end a symbol where the next one at the same or lower
indent starts, which is right for the usual one-declaration-per-block code."""
from __future__ import annotations

import ast
import re
from dataclasses import dataclass


@dataclass
class Symbol:
    name: str
    kind: str
    start: int
    end: int
    indent: int = 0
    marker: bool = False  # comment-derived, ends where the next symbol begins


_JS = re.compile(
    r"^(?P<i>\s*)(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(?:async\s+)?"
    r"(?:(?P<k>function\*?|class|interface|type|enum)\s+(?P<n>[A-Za-z_$][\w$]*)"
    r"|(?:const|let|var)\s+(?P<c>[A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s*)?(?:\(|function|[A-Za-z_$][\w$]*\s*=>|<))"
)
_METHOD = re.compile(r"^(?P<i>\s{2,4})(?:(?:public|private|protected|static|async|readonly|get|set)\s+)*(?P<n>[A-Za-z_$][\w$]*)\s*(?:<[^>]*>)?\([^;]*\)\s*(?::\s*[^={]+)?\{\s*$")
_GO = re.compile(r"^(?P<k>func|type)\s+(?:\([^)]*\)\s*)?(?P<n>\w+)")
_RS = re.compile(r"^(?P<i>\s*)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?P<k>fn|struct|enum|trait|impl|mod)\s+(?P<n>\w+)")
_MD = re.compile(r"^(?P<h>#{1,4})\s+(?P<n>.+)")
_JSX_NOTE = re.compile(r"^(?P<i>\s*)\{/\*\s*(?P<n>[^*]{3,80}?)\s*\*/\}\s*$")
_BANNER = re.compile(r"^(?P<i>\s*)//\s*[─\-=]{2,}\s*(?P<n>[^─\-=][^─=]{2,70}?)\s*[─\-=]*\s*$")
_NOT_METHODS = {"if", "for", "while", "switch", "catch", "return", "function", "constructor_"}


def _python(src: str) -> list[Symbol] | None:
    try:
        tree = ast.parse(src)
    except SyntaxError:
        return None
    out: list[Symbol] = []

    def walk(body, depth):
        for n in body:
            if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                kind = "class" if isinstance(n, ast.ClassDef) else "def"
                start = min([n.lineno] + [d.lineno for d in n.decorator_list])
                out.append(Symbol(n.name, kind, start, n.end_lineno or n.lineno, depth * 4))
                if isinstance(n, ast.ClassDef):
                    walk(n.body, depth + 1)

    walk(tree.body, 0)
    return out


def outline(path: str, src: str) -> list[Symbol]:
    ext = path.rsplit(".", 1)[-1].lower() if "." in path else ""
    if ext == "py":
        found = _python(src)
        if found is not None:
            return found
    lines = src.splitlines()
    syms: list[Symbol] = []
    for no, line in enumerate(lines, 1):
        if ext in {"ts", "tsx", "js", "jsx", "mjs", "cjs"}:
            m = _JS.match(line)
            if m:
                syms.append(Symbol(m.group("n") or m.group("c"), m.group("k") or "const", no, no, len(m.group("i"))))
                continue
            m = _METHOD.match(line)
            if m and m.group("n") not in _NOT_METHODS:
                syms.append(Symbol(m.group("n"), "method", no, no, len(m.group("i"))))
                continue
            m = _JSX_NOTE.match(line) or _BANNER.match(line)
            if m:  # markers inside huge components: the only map of where things render
                syms.append(Symbol(m.group("n"), "section", no, no, len(m.group("i")), marker=True))
        elif ext == "go":
            m = _GO.match(line)
            if m:
                syms.append(Symbol(m.group("n"), m.group("k"), no, no, 0))
        elif ext == "rs":
            m = _RS.match(line)
            if m:
                syms.append(Symbol(m.group("n"), m.group("k"), no, no, len(m.group("i"))))
        elif ext in {"md", "mdx"}:
            m = _MD.match(line)
            if m:
                syms.append(Symbol(m.group("n").strip(), "h" + str(len(m.group("h"))), no, no, len(m.group("h"))))
    for i, s in enumerate(syms):
        bound = next((t.start for t in syms[i + 1:] if t.indent <= s.indent), len(lines) + 1) - 1
        end = bound
        if not s.marker:
            end = _close(lines, s.start, bound)
        s.end = max(s.start, end)
        while s.end > s.start and not lines[s.end - 1].strip():
            s.end -= 1  # don't count trailing blank lines
    return syms


def _close(lines: list[str], start: int, bound: int) -> int:
    """Line where the declaration's brackets balance out (approximate: braces in strings are not parsed)."""
    bal, opened = 0, False
    for no in range(start, min(bound, len(lines)) + 1):
        line = lines[no - 1]
        for ch in line:
            if ch in "{([":
                bal, opened = bal + 1, True
            elif ch in "})]":
                bal -= 1
        if opened and bal <= 0:
            return no
        if not opened and line.rstrip().endswith(";"):
            return no
    return bound
