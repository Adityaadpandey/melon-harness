"""Problems a quick checker finds in a file the agent just changed, so a broken edit is caught in the same turn
instead of at the end of the run (opencode feeds language-server errors back the same way).

Only problems the change introduced are reported: the old and the new text are both checked and anything already
there before is left out, so a file with old lint never sends the model off fixing things nobody asked about.
Checkers are fast and local, and a missing one is simply skipped:
  .py          compile() for syntax, plus `ruff` (undefined names, syntax) when it is installed
  .json .toml  parsed in-process
  .ts .tsx .mts .cts  the project's own tsserver (node_modules/typescript), kept running: real type errors, ~0.3s warm
  .js .mjs .cjs `node --check` (JSX is left to the end-of-run eslint: node can't parse it)
  .go          `gofmt -e`
e.g. an edit that drops an import used further down comes back with
  <diagnostics file="app/models.py">  12:5 F821 Undefined name `Decimal`  </diagnostics>
"""
from __future__ import annotations

import asyncio
import atexit
import contextlib
import json
import re
import shutil
import tomllib
from pathlib import Path

TIMEOUT = 8.0
TS_TIMEOUT = 20.0  # the first check loads the whole project; a big monorepo takes seconds
MAX_PROBLEMS = 20
TS_SUFFIX = (".ts", ".tsx", ".mts", ".cts")


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


class TsServer:
    """The TypeScript language server every TS project already has, in node_modules/typescript/lib/tsserver.js.
    It stays up for the session, so after the first check (which loads the project) each one answers in well under a
    second with the errors `tsc` would print: a wrong type, a missing import, a call with too few arguments.
    A timeout (a big project still loading) skips that edit's check but leaves the server loading for the next one;
    only a dead server is restarted."""

    def __init__(self, script: Path):
        self.script, self.proc, self.seq, self.lock = script, None, 0, asyncio.Lock()
        self.pending: dict[int, asyncio.Future] = {}
        self.reader: asyncio.Task | None = None

    async def _start(self, cwd: Path) -> None:
        if self.proc is None or self.proc.returncode is not None:
            self.proc = await asyncio.create_subprocess_exec(
                "node", str(self.script), "--disableAutomaticTypingAcquisition", "--suppressDiagnosticEvents", cwd=cwd,
                stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
                start_new_session=True,
            )
            self.reader = asyncio.create_task(self._read())

    async def _read(self) -> None:
        """Hand each response to whoever asked for it. Messages arrive framed as `Content-Length: N`, a blank line,
        then N bytes of JSON; events and answers nobody waits for any more are dropped."""
        stdout = self.proc.stdout
        try:
            while line := await stdout.readline():
                if line.startswith(b"Content-Length:"):
                    n = int(line.split(b":", 1)[1])
                    await stdout.readline()
                    msg = json.loads(await stdout.readexactly(n))
                    waiter = self.pending.pop(msg.get("request_seq"), None) if msg.get("type") == "response" else None
                    if waiter and not waiter.done():
                        waiter.set_result(msg.get("body") or [])
        except (ValueError, asyncio.IncompleteReadError):
            pass
        for waiter in self.pending.values():
            if not waiter.done():
                waiter.set_exception(ConnectionError("tsserver exited"))
        self.pending.clear()

    def _send(self, command: str, arguments: dict, reply: bool = True) -> asyncio.Future | None:
        self.seq += 1
        waiter = self.pending[self.seq] = asyncio.get_running_loop().create_future() if reply else None
        msg = {"seq": self.seq, "type": "request", "command": command, "arguments": arguments}
        self.proc.stdin.write((json.dumps(msg) + "\n").encode())
        return waiter

    async def _ask(self, command: str, arguments: dict):
        waiter = self._send(command, arguments)
        try:
            return await asyncio.wait_for(asyncio.shield(waiter), TS_TIMEOUT)
        except asyncio.TimeoutError:
            waiter.cancel()  # its answer will be dropped when it comes
            raise

    async def errors(self, path: Path, text: str, cwd: Path) -> list[str]:
        async with self.lock:
            try:
                await self._start(cwd)
                file = str(path.resolve())
                self._send("updateOpen", {"openFiles": [{"file": file, "fileContent": text}]}, reply=False)
                found = []
                for command in ("syntacticDiagnosticsSync", "semanticDiagnosticsSync"):
                    body = await self._ask(command, {"file": file})
                    found += [f"{d['start']['line']}:{d['start']['offset']} TS{d.get('code', '')}: {d['text']}"
                              for d in body if d.get("category", "error") == "error"]
                # leave tsserver on what is really on disk, or the next file's check would see this one's old text
                on_disk = path.read_text(encoding="utf-8", errors="replace") if path.exists() else ""
                if on_disk != text:
                    self._send("updateOpen", {"openFiles": [{"file": file, "fileContent": on_disk}]}, reply=False)
                return found
            except asyncio.TimeoutError:
                return []  # still loading the project: the next edit gets an answer
            except (OSError, ConnectionError, ValueError):
                self.close()  # dead: restarted on the next edit
                return []

    def close(self) -> None:
        if self.proc and self.proc.returncode is None:
            with contextlib.suppress(ProcessLookupError):
                self.proc.kill()
        if self.reader and not self.reader.done():
            self.reader.cancel()
        self.proc, self.reader = None, None


_TS: dict[tuple[str, int], TsServer] = {}  # (tsserver.js, event loop): a process belongs to the loop that started it


def find_tsserver(path: Path, cwd: Path) -> Path | None:
    """The tsserver of the package the file lives in, looking up from its folder to the workspace root."""
    root = cwd.resolve()
    for d in path.resolve().parents:
        script = d / "node_modules" / "typescript" / "lib" / "tsserver.js"
        if script.is_file():
            return script
        if d == root or root not in d.parents:
            return None
    return None


async def _typescript(path: Path, text: str, cwd: Path) -> list[str]:
    script = find_tsserver(path, cwd)
    if script is None or not shutil.which("node"):
        return []  # no TypeScript installed here: eslint at the end of the run still looks
    key = (str(script), id(asyncio.get_running_loop()))
    server = _TS.get(key) or _TS.setdefault(key, TsServer(script))
    return await server.errors(path, text, cwd)


@atexit.register
def _stop_servers() -> None:
    for server in _TS.values():
        server.close()


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
    if suffix in TS_SUFFIX:
        return await _typescript(path, text, cwd)
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
