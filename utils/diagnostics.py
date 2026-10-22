"""Problems a quick checker finds in a file the agent just changed, so a broken edit is caught in the same turn
instead of at the end of the run (opencode feeds language-server errors back the same way).

Only problems the change introduced are reported: the old and the new text are both checked and anything already
there before is left out, so a file with old lint never sends the model off fixing things nobody asked about.
Checkers are fast and local, and a missing one is simply skipped:
  .py          compile() for syntax, plus `ruff` (undefined names, syntax) when it is installed
  .json .toml  parsed in-process
  .js .mjs .cjs `node --check` (TypeScript and JSX are left to the end-of-run eslint: node can't parse them)
  .go          `gofmt -e`
e.g. an edit that drops an import used further down comes back with
  <diagnostics file="app/models.py">  12:5 F821 Undefined name `Decimal`  </diagnostics>
"""
from __future__ import annotations

import asyncio
import json
import re
import shutil
import tomllib
from pathlib import Path

TIMEOUT = 8.0
MAX_PROBLEMS = 20


async def _run(cmd: list[str], text: str, cwd: Path) -> tuple[int | None, str]:
    try:
        proc = await asyncio.create_subprocess_exec(
            *cmd, cwd=cwd, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )
        out, err = await asyncio.wait_for(proc.communicate(text.encode()), TIMEOUT)
    except (OSError, asyncio.TimeoutError):
        return None, ""  # no checker, or too slow: say nothing rather than guess
    return proc.returncode, (out + err).decode("utf-8", "replace")


async def _python(path: Path, text: str, cwd: Path) -> list[str]:
    try:
        compile(text, str(path), "exec", dont_inherit=True)
    except SyntaxError as e:
        return [f"{e.lineno}:{e.offset or 0} SyntaxError: {e.msg}"]
    except ValueError:
        return []
    ruff = shutil.which("ruff") or next((str(p) for p in (cwd / ".venv/bin/ruff",) if p.exists()), None)
    if not ruff:
        return []
    _, out = await _run([ruff, "check", "--no-cache", "--quiet", "--select", "E9,F63,F7,F82", "--output-format", "concise",
                         "--stdin-filename", str(path), "-"], text, cwd)
    return [m.group(1) for m in re.finditer(r"^.*?:(\d+:\d+: .+)$", out, re.M)]


async def _node(path: Path, text: str, cwd: Path) -> list[str]:
    if re.search(r"</|/>", text):
        return []  # JSX: not plain JavaScript, node would call every tag an error
    esm = path.suffix == ".mjs" or (path.suffix == ".js" and re.search(r"^\s*(import|export)\b", text, re.M))
    code, out = await _run(["node", "--no-warnings", "--check", f"--input-type={'module' if esm else 'commonjs'}", "-"], text, cwd)
    if not code:
        return []
    line = re.search(r"\[stdin\]:(\d+)", out)
    msg = re.search(r"^(\w*Error.*)$", out, re.M)
    return [f"{line.group(1) if line else '?'}:0 {msg.group(1) if msg else out.strip().splitlines()[-1]}"]


async def _go(path: Path, text: str, cwd: Path) -> list[str]:
    if not shutil.which("gofmt"):
        return []
    code, out = await _run(["gofmt", "-e"], text, cwd)
    return [m.group(1) for m in re.finditer(r"^<standard input>:(\d+:\d+: .+)$", out, re.M)] if code else []


def _data(path: Path, text: str) -> list[str]:
    try:
        json.loads(text) if path.suffix == ".json" else tomllib.loads(text)
    except json.JSONDecodeError as e:
        return [f"{e.lineno}:{e.colno} {e.msg}"]
    except tomllib.TOMLDecodeError as e:
        return [f"0:0 {e}"]
    return []


async def problems(path: Path, text: str, cwd: Path) -> list[str]:
    suffix = path.suffix.lower()
    if suffix == ".py":
        return await _python(path, text, cwd)
    if suffix in (".js", ".mjs", ".cjs") and shutil.which("node"):
        return await _node(path, text, cwd)
    if suffix == ".go":
        return await _go(path, text, cwd)
    if suffix in (".json", ".toml"):
        return _data(path, text)
    return []


def _key(problem: str) -> str:
    return re.sub(r"^[\d?]+:\d+:? ", "", problem)  # the same error moved down by an insert is still the same error


async def new_problems(path: Path, old: str | None, new: str, cwd: Path) -> list[str]:
    """Problems in `new` that `old` did not have, line:col first, at most MAX_PROBLEMS."""
    after = await problems(path, new, cwd)
    if not after:
        return []
    before = {_key(p) for p in await problems(path, old, cwd)} if old else set()
    return [p for p in after if _key(p) not in before][:MAX_PROBLEMS]


def render(rel: str, found: list[str]) -> str:
    lines = "\n".join(f"  {p}" for p in found)
    return (f'\n\n<diagnostics file="{rel}">\nYour change left these problems in the file; fix them before moving on:\n'
            f"{lines}\n</diagnostics>")
