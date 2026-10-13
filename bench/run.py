"""Benchmark runner.

  python -m bench.run --harness . --label new --model claude-haiku-5-5 --runs 3
  python -m bench.run --harness /path/to/old-checkout --label baseline --model deepseek/deepseek-v4-flash
  python -m bench.run --summarize bench/results/*.json

Every run gets a fresh git repo built from the task fixture, the harness is run
as a subprocess exactly like a user would, and acceptance uses hidden tests the
agent never saw. Tasks the harness could not run (no credentials, quota, model
not in plan) are recorded as `blocked`, never as passes or fails."""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from dotenv import load_dotenv

from bench.tasks import ALL, BY_ID, Task

ROOT = Path(__file__).resolve().parent.parent
BLOCK_MARKERS = ("MODEL_NOT_IN_PLAN", "No API key", "must be called via", "Rate limit exceeded", "usage limit", "limit resets")


def build_repo(task: Task, d: Path, model: str, legacy: bool) -> None:
    for rel, content in task.files.items():
        f = d / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(content, encoding="utf-8")
    (d / ".gitignore").write_text("__pycache__/\n.pytest_cache/\n*.pyc\n")
    (d / ".ai-agent").mkdir()
    if legacy:
        cfg = f'approval = "auto"\nmax_turns = 40\n[model]\nname = "{model}"\ntemperature = 0\n'
    else:
        cfg = f'max_turns = 40\nmax_seconds = 540\ntrace = true\n[model]\nname = "{model}"\n'
    (d / ".ai-agent" / "config.toml").write_text(cfg)
    for cmd in (["init", "-q"], ["config", "user.email", "b@b"], ["config", "user.name", "bench"],
                ["add", "-A"], ["commit", "-qm", "fixture"]):
        subprocess.run(["git", *cmd], cwd=d, check=True, capture_output=True)


def accept(task: Task, d: Path, env: dict) -> tuple[bool, str]:
    for rel, content in task.hidden.items():
        f = d / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(content)
    r = subprocess.run(task.accept, shell=True, cwd=d, env=env, capture_output=True, text=True, timeout=180)
    return r.returncode == 0, (r.stdout + r.stderr)[-600:]


async def run_one(task: Task, harness: Path, model: str, mode: str, timeout: int, env: dict, run_idx: int) -> dict:
    legacy = not (harness / "agent" / "task.py").exists()
    d = Path(tempfile.mkdtemp(prefix=f"bench-{task.id}-"))
    build_repo(task, d, model, legacy)
    originals = {p: (d / p).read_bytes() for p in task.protect}
    report_path = d.parent / f"{d.name}.report.json"

    cmd = [sys.executable, str(harness / "main.py")]
    prompt = task.prompt
    if not legacy:
        cmd += ["-m", model, "--approval", "auto", "--report", str(report_path)]
        if mode == "parallel" and task.subtasks:
            graph = d.parent / f"{d.name}.graph.json"
            graph.write_text(json.dumps({"tasks": task.subtasks}))
            cmd += ["--graph", str(graph)]
    cmd.append(prompt)

    t0 = time.perf_counter()
    timed_out = False
    proc = await asyncio.create_subprocess_exec(
        *cmd, cwd=d, env=env, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT,
        start_new_session=True,
    )
    try:
        out, _ = await asyncio.wait_for(proc.communicate(), timeout)
    except asyncio.TimeoutError:
        timed_out = True
        os.killpg(os.getpgid(proc.pid), 9)
        out, _ = await proc.communicate()
    wall = time.perf_counter() - t0
    text = out.decode("utf-8", "replace")

    rep = {}
    if report_path.exists():
        try:
            rep = json.loads(report_path.read_text())
        except ValueError:
            pass
    blocked = any(m in text for m in BLOCK_MARKERS) and not rep.get("model_calls")

    acc_ok, acc_tail = accept(task, d, env)
    protect_ok = all((d / p).exists() and (d / p).read_bytes() == b for p, b in originals.items())
    seen = text + str(rep.get("final_response") or "")
    stdout_ok = all(s.lower() in seen.lower() for s in task.stdout_contains)
    success = acc_ok and protect_ok and stdout_ok and not blocked

    usage = rep.get("usage") or {}
    row = {
        "task": task.id, "category": task.category, "model": model, "mode": mode, "run": run_idx,
        "status": "blocked" if blocked else ("pass" if success else "fail"),
        "accept_ok": acc_ok, "protect_ok": protect_ok, "stdout_ok": stdout_ok,
        "regression": not protect_ok,
        "claimed_done": proc.returncode == 0 and not timed_out,
        "exit_code": proc.returncode, "timed_out": timed_out, "wall_sec": round(wall, 2),
        "harness_status": (rep.get("report") or {}).get("status"),
        "repairs": (rep.get("report") or {}).get("repairs"),
        "model_calls": rep.get("model_calls"), "tool_calls": rep.get("tool_calls"),
        "tool_failures": rep.get("tool_failures"), "model_call_failures": rep.get("model_call_failures"),
        "prompt_tokens": usage.get("prompt_tokens"), "completion_tokens": usage.get("completion_tokens"),
        "cached_tokens": usage.get("cached_tokens"),
        "models_used": rep.get("models_used"), "out_tail": text[-1500:],
        "fail_detail": None if success else (text[-500:] if not rep else (acc_tail if not acc_ok else text[-400:])),
    }
    shutil.rmtree(d, ignore_errors=True)
    report_path.unlink(missing_ok=True)
    return row


async def main_run(a) -> None:
    load_dotenv(ROOT / ".env")
    env = os.environ.copy()
    env["PATH"] = f"{Path(sys.executable).parent}:{env['PATH']}"
    env.setdefault("BASE_URL", "https://api.commandcode.ai/provider/v1")
    harness = Path(a.harness).resolve()
    tasks = [BY_ID[t] for t in a.tasks.split(",")] if a.tasks else [
        t for t in ALL if a.mode == "single" or t.subtasks
    ]
    sem = asyncio.Semaphore(a.jobs)

    async def guarded(t, i):
        async with sem:
            row = await run_one(t, harness, a.model, a.mode, a.timeout, env, i)
            row["label"], row["jobs"] = a.label, a.jobs
            print(f"{row['status']:<7} {t.id:<28} {row['wall_sec']:>7}s  {a.model}", flush=True)
            return row

    rows = await asyncio.gather(*(guarded(t, i) for t in tasks for i in range(a.runs)))
    out = Path(a.out or ROOT / "bench" / "results" / f"{a.label}-{a.model.replace('/', '_')}-{a.mode}-{int(time.time())}.json")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(rows, indent=1))
    print(f"\nwrote {out}")
    summarize([out])


def pct(vals, p):
    vals = sorted(vals)
    return vals[min(len(vals) - 1, int(round(p * (len(vals) - 1))))] if vals else float("nan")


def summarize(paths) -> None:
    rows = [r for p in paths for r in json.loads(Path(p).read_text())]
    groups: dict[tuple, list] = {}
    for r in rows:
        groups.setdefault((r["label"], r["model"], r["mode"]), []).append(r)
    print(f"\n{'label':<10}{'model':<30}{'mode':<9}{'pass':>8}{'blocked':>8}{'regr':>6}{'falseclaim':>11}"
          f"{'p50s':>8}{'p95s':>8}{'calls':>7}{'tok_in':>9}{'tok_out':>9}")
    for (label, model, mode), rs in sorted(groups.items()):
        ran = [r for r in rs if r["status"] != "blocked"]
        passed = sum(r["status"] == "pass" for r in ran)
        fc = sum(1 for r in ran if r["claimed_done"] and r["status"] == "fail")
        walls = [r["wall_sec"] for r in ran]
        s = lambda k: sum(r[k] or 0 for r in ran)
        print(f"{label:<10}{model:<30}{mode:<9}{f'{passed}/{len(ran)}':>8}{len(rs) - len(ran):>8}"
              f"{sum(r['regression'] for r in ran):>6}{fc:>11}{statistics.median(walls) if walls else 0:>8.1f}"
              f"{pct(walls, .95):>8.1f}{s('model_calls'):>7}{s('prompt_tokens'):>9}{s('completion_tokens'):>9}")
    print("\nper task (pass/ran):")
    tasks = sorted({r["task"] for r in rows})
    keys = sorted(groups)
    print(f"{'task':<30}" + "".join(f"{k[0][:8]}/{k[1].split('/')[-1][:14]}/{k[2][:3]:<5}" for k in keys))
    for t in tasks:
        line = f"{t:<30}"
        for k in keys:
            rs = [r for r in groups[k] if r["task"] == t and r["status"] != "blocked"]
            line += f"{sum(r['status'] == 'pass' for r in rs)}/{len(rs)}".ljust(len(f"{k[0][:8]}/{k[1].split('/')[-1][:14]}/{k[2][:3]:<5}"))
        print(line)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--harness", default=str(ROOT))
    ap.add_argument("--label", default="new")
    ap.add_argument("--model")
    ap.add_argument("--tasks")
    ap.add_argument("--runs", type=int, default=1)
    ap.add_argument("--mode", choices=["single", "parallel"], default="single")
    ap.add_argument("--jobs", type=int, default=3)
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--out")
    ap.add_argument("--summarize", nargs="*")
    a = ap.parse_args()
    if a.summarize is not None:
        summarize(a.summarize)
    else:
        if not a.model:
            ap.error("--model is required")
        asyncio.run(main_run(a))
