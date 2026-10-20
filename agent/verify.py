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
    return sorted(p for p, h in after.items() if before.get(p) != h) + sorted(
        p for p in before if p not in after  # reverted to clean = also a change
    )


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
