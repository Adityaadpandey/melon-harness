"""Change detection and the verification commands run after an agent edits files."""
from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import os
import re
import shlex
import signal
from dataclasses import dataclass
from pathlib import Path

from tools.builtin.shell import build_environment

MAX_HASH_BYTES = 5_000_000
OUTPUT_TAIL = 4000


async def _git(cwd: Path, *args: str) -> tuple[int, bytes]:
    proc = await asyncio.create_subprocess_exec(
        "git", *args, cwd=cwd,
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
    )
    out, _ = await proc.communicate()
    return proc.returncode, out


async def snapshot(cwd: Path) -> dict[str, str] | None:
    """path -> content hash for every dirty/untracked file, or None outside git.
    Comparing two snapshots isolates what *this run* changed from edits the
    user already had in flight."""
    code, out = await _git(cwd, "status", "--porcelain=v1", "-z", "-uall")
    if code != 0:
        return None
    snap: dict[str, str] = {}
    entries = out.decode("utf-8", "replace").split("\0")
    i = 0
    while i < len(entries):
        e = entries[i]
        i += 1
        if len(e) < 4:
            continue
        status, path = e[:2], e[3:]
        if status[0] in "RC":  # rename/copy entries carry the old path next
            i += 1
        f = cwd / path
        try:
            if f.is_file() and f.stat().st_size <= MAX_HASH_BYTES:
                snap[path] = hashlib.sha1(f.read_bytes()).hexdigest()
            else:
                snap[path] = "deleted" if not f.exists() else "large"
        except OSError:
            snap[path] = "unreadable"
    return snap


def changed_since(before: dict[str, str] | None, after: dict[str, str] | None) -> list[str]:
    if before is None or after is None:
        return []
    # a file that went from dirty to clean is not listed: after "commit this" every committed file looks reverted,
    # and listing them sent a plain commit through checks, a review and a repair that edited code
    return sorted(p for p, h in after.items() if before.get(p) != h)


JS_SUFFIX = {".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"}


def _package_runner(cwd: Path, pkg_dir: Path) -> str:
    for d in (pkg_dir, *pkg_dir.parents):
        for lock, runner in (("pnpm-lock.yaml", "pnpm"), ("yarn.lock", "yarn"), ("bun.lockb", "bun"), ("bun.lock", "bun"), ("package-lock.json", "npm")):
            if (d / lock).exists():
                return runner
        if d == cwd:
            break
    return "npm"


def _py_tests(cwd: Path, f: str) -> list[str]:
    """The test file for a changed module: itself if it is one, else test_<name>.py next to it or under tests/."""
    p = Path(f)
    if p.name.startswith("test_") or p.stem.endswith("_test"):
        return [f]
    found = [p.parent / f"test_{p.name}"] + (sorted((cwd / "tests").rglob(f"test_{p.name}")) if (cwd / "tests").is_dir() else [])
    return sorted({(t.relative_to(cwd) if t.is_absolute() else t).as_posix() for t in found if (cwd / t).is_file()})


def _eslint(cwd: Path, files: list[str]) -> list[tuple[str, str]]:
    """eslint on just the changed files, per package, errors only. Nothing when eslint is not installed."""
    by_pkg: dict[Path, list[str]] = {}
    for f in files:
        d = (cwd / f).parent
        while not (d / "package.json").is_file() and d != cwd and cwd in d.parents:
            d = d.parent
        by_pkg.setdefault(d, []).append(os.path.relpath(cwd / f, d))
    out = []
    for d, rel in by_pkg.items():
        if any((x / "node_modules" / ".bin" / "eslint").exists() for x in (d, cwd)):
            out.append((f"{_package_runner(cwd, d)} exec eslint --quiet " + " ".join(shlex.quote(r) for r in rel),
                        d.relative_to(cwd).as_posix() or "."))
    return out


def detect_checks(cwd: Path, changed: list[str]) -> list[tuple[str, str]]:
    """One quick check of the files this run changed, never a build, the whole suite or a project-wide typecheck:
    the user asked for that, those ran for minutes per request (and up to 3 times with repairs).
    Example: a.py and web/x.tsx changed -> `pytest -q -x tests/test_a.py` and `pnpm exec eslint --quiet src/x.tsx` in web.
    The wider checks are the user's to run; the model names them in its final answer."""
    cwd = cwd.resolve()
    live = [f for f in changed if (cwd / f).is_file()]
    checks: list[tuple[str, str]] = []
    py = [f for f in live if f.endswith(".py")]
    if py:
        tests = sorted({t for f in py for t in _py_tests(cwd, f)})
        checks.append(("python3 -m pytest -q -x " + " ".join(map(shlex.quote, tests)) if tests
                       else "python3 -m py_compile " + " ".join(map(shlex.quote, py)), "."))
    go = sorted({"./" + Path(f).parent.as_posix() for f in live if f.endswith(".go")})
    if go and (cwd / "go.mod").exists():
        checks.append(("go vet " + " ".join(go), "."))
    if any(f.endswith(".rs") for f in live) and (cwd / "Cargo.toml").exists():
        checks.append(("cargo check -q", "."))  # rust cannot check a single file; check is still far short of a test run
    checks += _eslint(cwd, [f for f in live if Path(f).suffix in JS_SUFFIX])
    return checks


def related_tests(root: Path, rule, hits: list[str]) -> list[str]:
    """Specs (under the rule's cwd, matching tests_glob) that import one of the changed files."""
    if not rule.tests_glob:
        return []
    base = root / rule.cwd
    stems = {Path(f).stem for f in hits if not re.search(r"\.(spec|test)\.", f)}
    found = []
    for spec in sorted(base.glob(rule.tests_glob)):
        try:
            text = spec.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if any(re.search(rf"/{re.escape(st)}[\"']", text) for st in stems):
            found.append(spec.relative_to(base).as_posix())
    return found[:8]


def select_checks(config, changed: list[str]) -> list[tuple[str, str]]:
    """(command, cwd) pairs to run for this change set. Explicit commands win,
    then rules matching the changed paths (monorepos), then auto-detection."""
    checks: list[tuple[str, str]] = [(c, ".") for c in config.verify_commands]
    for rule in config.verify_rules:
        hits = [f for f in changed if any(f.startswith(p) for p in rule.paths)]
        if not hits:
            continue
        rel = [os.path.relpath(f, rule.cwd) for f in hits]
        files = " ".join(shlex.quote(f) for f in rel)
        tests = " ".join(shlex.quote(t) for t in related_tests(config.cwd, rule, hits))
        for cmd in rule.commands:
            if "{tests}" in cmd and not tests:
                continue
            item = (cmd.replace("{files}", files).replace("{tests}", tests), rule.cwd)
            if item not in checks:
                checks.append(item)
    if not checks and config.auto_verify:
        checks = detect_checks(config.cwd, changed)
    return checks


@dataclass
class CheckResult:
    command: str
    ok: bool
    exit_code: int | None
    seconds: float
    output_tail: str

    def to_dict(self) -> dict:
        return {
            "command": self.command, "ok": self.ok, "exit_code": self.exit_code,
            "seconds": round(self.seconds, 2),
        }


async def run_check(command: str, cwd: Path, config) -> CheckResult:
    loop = asyncio.get_event_loop()
    t = loop.time()
    from safety.sandbox import wrap

    proc = await asyncio.create_subprocess_exec(
        *wrap(["/bin/bash", "-c", command], config), cwd=cwd, env=build_environment(config),
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT,
        start_new_session=True,
    )
    try:
        out, _ = await asyncio.wait_for(proc.communicate(), config.verify_timeout_sec)
        text = out.decode("utf-8", "replace")
        return CheckResult(command, proc.returncode == 0, proc.returncode, loop.time() - t, text[-OUTPUT_TAIL:])
    except asyncio.CancelledError:
        # the user steered mid-check: a typecheck of the old diff is not worth finishing, and must not linger
        with contextlib.suppress(ProcessLookupError):
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        raise
    except asyncio.TimeoutError:
        os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        await proc.wait()
        return CheckResult(
            command, False, None, loop.time() - t,
            f"timed out after {config.verify_timeout_sec}s",
        )


def repair_prompt(failures: list[CheckResult], files: list[str]) -> str:
    blocks = "\n\n".join(
        f"$ {f.command}\n(exit {f.exit_code})\n{f.output_tail}" for f in failures
    )
    return (
        "Automated verification FAILED after your changes "
        f"(files touched: {', '.join(files[:20])}).\n\n{blocks}\n\n"
        "Find the root cause and make the smallest fix. Do not weaken or delete "
        "a test just to make it pass; only change a test if its expectation is "
        "genuinely wrong, and say so. Stop when the checks should pass."
    )


async def backup(cwd: Path) -> str | None:
    """A commit holding the working tree as it is right now (tracked files, staged
    or not), made without touching the tree, index or stash list. Clean tree: HEAD.
    None outside git or in a repo with no commits."""
    code, out = await _git(cwd, "stash", "create")
    sha = out.decode().strip() if code == 0 else ""
    if not sha:
        code, out = await _git(cwd, "rev-parse", "--verify", "HEAD")
        sha = out.decode().strip() if code == 0 else ""
    if sha:  # keep the objects from being garbage collected while the user may still undo
        await _git(cwd, "update-ref", "refs/melon/backup", sha)
    return sha or None


async def undo(cwd: Path, ref: str, changed: list[str], created: list[str]) -> tuple[list[str], list[str], list[str]]:
    """Put back what a run changed: (restored, deleted, skipped). Files the run
    created are deleted; files that existed untracked before the run have no
    copy in `ref`, so they are reported, not touched."""
    restored, deleted, skipped = [], [], []
    for f in changed:
        code, _ = await _git(cwd, "cat-file", "-e", f"{ref}:{f}")
        if code == 0:
            code, _ = await _git(cwd, "restore", f"--source={ref}", "--worktree", "--", f)
            (restored if code == 0 else skipped).append(f)
        elif f in created and (cwd / f).is_file():
            (cwd / f).unlink()
            deleted.append(f)
        else:
            skipped.append(f)
    return restored, deleted, skipped


# --- signature guard -------------------------------------------------------------------------------------------
# Checks prove the code runs; they can't see a caller that isn't in the repo or in the tests the agent knows of.
# The benchmark case: asked to fix `def unique_by(items, key, seen=set())`, the agent dropped `seen` altogether,
# its own tests passed, and every caller that passed seen= got a TypeError. These helpers list such breaks so the
# agent is asked once to keep old calls working (or to say why it can't).

_TEST_FILE = re.compile(r"(^|/)(tests?|__tests__)/|(^|/)test_[^/]*\.py$|_test\.py$|\.(test|spec)\.[jt]sx?$")


def _py_signatures(src: str) -> dict[str, "ast.arguments"] | None:
    """Public functions and methods by qualified name ("Cart.add"), or None when the file doesn't parse."""
    import ast

    try:
        tree = ast.parse(src)
    except SyntaxError:
        return None
    sigs: dict[str, ast.arguments] = {}

    def walk(body, prefix: str) -> None:
        for n in body:
            if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)) and (not n.name.startswith("_") or n.name == "__init__"):
                sigs[prefix + n.name] = n.args
            elif isinstance(n, ast.ClassDef) and not n.name.startswith("_"):
                walk(n.body, f"{prefix}{n.name}.")

    walk(tree.body, "")
    return sigs


def _py_breaks(old_src: str, new_src: str) -> tuple[list[tuple[str, str]], set[str]]:
    """([(what broke, the name it is about)], names still defined) for one Python file."""
    old, new = _py_signatures(old_src), _py_signatures(new_src)
    if old is None or new is None:
        return [], set()
    out = []
    for name, a in old.items():
        b = new.get(name)
        if b is None:
            out.append((f"{name}() was removed or renamed", name.rsplit(".", 1)[-1]))
            continue
        old_pos = [x.arg for x in a.posonlyargs + a.args]
        new_pos = [x.arg for x in b.posonlyargs + b.args]
        old_all = old_pos + [x.arg for x in a.kwonlyargs]
        new_all = new_pos + [x.arg for x in b.kwonlyargs]
        if b.kwarg is None:
            out += [(f"{name}() no longer accepts `{p}`", p) for p in old_all if p not in new_all]
        n_pos_defaults = len(b.defaults)
        required = new_pos[: len(new_pos) - n_pos_defaults] + [k.arg for k, d in zip(b.kwonlyargs, b.kw_defaults) if d is None]
        out += [(f"{name}() now requires `{p}`", p) for p in required if p not in old_all]
        moved = [p for i, p in enumerate(old_pos) if p in new_pos and new_pos.index(p) != i and p not in ("self", "cls")]
        if moved:
            out.append((f"{name}() reorders its positional parameters ({', '.join(old_pos)} -> {', '.join(new_pos)})", moved[0]))
        if a.vararg and not b.vararg:
            out.append((f"{name}() no longer accepts *{a.vararg.arg}", a.vararg.arg))
        if a.kwarg and not b.kwarg:
            out.append((f"{name}() no longer accepts **{a.kwarg.arg}", a.kwarg.arg))
    return out, {n.rsplit(".", 1)[-1] for n in new}


_JS_EXPORT = re.compile(
    r"export\s+(?:default\s+)?(?:async\s+)?function\s*\*?\s*(\w+)\s*(?:<[^>()]*>)?\s*\("
    r"|export\s+const\s+(\w+)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\s*\w*\s*)?(?:<[^>()]*>)?\s*\("
)


def _js_signatures(src: str) -> dict[str, list[tuple[str, bool]]]:
    """Exported functions -> [(parameter text, optional)]. A light reader, not a TS parser: enough for
    `export function f(a: string, b = 1)` and `export const f = async ({ id }: Props) =>`."""
    sigs: dict[str, list[tuple[str, bool]]] = {}
    for m in _JS_EXPORT.finditer(src):
        depth, i, start = 1, m.end(), m.end()
        while i < len(src) and depth:
            depth += {"(": 1, "[": 1, "{": 1, ")": -1, "]": -1, "}": -1}.get(src[i], 0)
            i += 1
        params, cur, d = [], "", 0
        for ch in src[start:i - 1] + ",":
            if ch == "," and d == 0:
                if cur.strip():
                    p = cur.strip()
                    top = re.sub(r"[\[{(<].*", "", p)  # what sits before any nested type or pattern
                    params.append((p, p.startswith("...") or "?" in top or "=" in re.sub(r"=>", "", p)))
                cur = ""
                continue
            d += {"(": 1, "[": 1, "{": 1, "<": 1, ")": -1, "]": -1, "}": -1, ">": -1}.get(ch, 0) if not (ch == ">" and cur.endswith("=")) else 0
            cur += ch
        sigs[m.group(1) or m.group(2)] = params
    return sigs


def _js_breaks(old_src: str, new_src: str) -> tuple[list[tuple[str, str]], set[str]]:
    old, new = _js_signatures(old_src), _js_signatures(new_src)
    out = []
    for name, a in old.items():
        b = new.get(name)
        if b is None:
            if not re.search(rf"export\s[^;\n]*\b{re.escape(name)}\b", new_src):  # still exported some other way: fine
                out.append((f"{name}() is no longer exported", name))
            continue
        if len(b) < len(a) and not (b and b[-1][0].startswith("...")):
            out.append((f"{name}() takes fewer parameters ({len(a)} -> {len(b)})", name))
        if sum(not opt for _, opt in b) > sum(not opt for _, opt in a):
            out.append((f"{name}() requires more arguments than before", name))
    return out, set(new)


async def signature_breaks(cwd: Path, changed: list[str], request: str = "") -> list[str]:
    """Public signatures this change breaks, as "file: what", e.g. "lib/dedupe.py: unique_by() no longer accepts `seen`".
    Skips test files, new and deleted files, a function that moved to another changed file, and anything whose name the
    request mentions (the user asked for it)."""
    found: list[tuple[str, str, str]] = []
    defined: set[str] = set()
    asked = set(re.findall(r"\w+", request.lower()))
    for f in changed:
        suffix = Path(f).suffix
        if (suffix != ".py" and suffix not in JS_SUFFIX) or _TEST_FILE.search(f) or not (cwd / f).is_file():
            continue
        code, old = await _git(cwd, "show", f"HEAD:{f}")
        if code != 0:
            continue
        try:
            new = (cwd / f).read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        breaks, names = (_py_breaks if suffix == ".py" else _js_breaks)(old.decode("utf-8", "replace"), new)
        defined |= names
        found += [(f, what, about) for what, about in breaks]
    return [f"{f}: {what}" for f, what, about in found
            if about.lower() not in asked and not (("removed" in what or "exported" in what) and about in defined)]


def signature_prompt(breaks: list[str]) -> str:
    return (
        "Your change alters public signatures that code outside this diff may rely on (other modules, other repos, "
        "callers no test here covers):\n" + "\n".join(f"- {b}" for b in breaks[:10]) + "\n\n"
        "Unless the user asked for that, keep old calls working: keep the parameter and give it a safe default "
        "(e.g. `seen=None`, then create the value inside), or keep the old name as a thin wrapper. Then re-run the check. "
        "If the break is really needed, leave it and say so in one sentence of your final answer."
    )
