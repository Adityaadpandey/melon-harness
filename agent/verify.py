"""Change detection and the verification commands run after an agent edits files."""
from __future__ import annotations

import asyncio
import hashlib
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


def detect_verify_commands(cwd: Path) -> list[str]:
    """Conservative auto-detection. JS/TS and anything needing services is left
    to explicit config: guessing `npm test` can hit databases or the network."""
    if (cwd / "go.mod").exists():
        return ["go test ./..."]
    if (cwd / "Cargo.toml").exists():
        return ["cargo test"]
    has_py_tests = (cwd / "tests").is_dir() or any(cwd.glob("test_*.py"))
    if has_py_tests or (cwd / "pytest.ini").exists():
        return ["python3 -m pytest -q -x"]
    return []


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
        checks = [(c, ".") for c in detect_verify_commands(config.cwd)]
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
        await _git(cwd, "update-ref", "refs/mew/backup", sha)
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
