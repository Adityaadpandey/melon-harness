"""Offline replay of an incident-shaped review: many reads and greps, then the same
files read again. No model and no API key, so it costs nothing and gives the same
numbers every run.

  python -m bench.context_replay --label baseline
  python -m bench.context_replay --label after --compact-at 200000

The scripted "model" calls the same tools in the same order whatever the harness
does, so any difference between two runs comes from the harness: what it keeps in
the request, and whether it rewrites old messages (which breaks a provider's prompt
cache). Cache reads are estimated from the longest prefix a request shares with the
one before it, so every number here is an estimate, not provider usage."""
from __future__ import annotations

import argparse
import asyncio
import json
import statistics
import subprocess
import tempfile
from pathlib import Path

from agent.agent import Agent
from client.response import StreamEvent, StreamEventType, TokenUsage
from config.config import ApprovalPolicy, Config
from tests.fakes import ScriptedClient, calls, text

FILES = 24
LINES = 280


def build_fixture(root: Path) -> None:
    for i in range(FILES):
        body = "\n".join(f"export const value_{i}_{n} = computeSomething({n}, 'file {i} line {n}');" for n in range(LINES))
        f = root / "src" / f"module_{i}.ts"
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(body + "\n")
    for cmd in (["init", "-q"], ["config", "user.email", "b@b"], ["config", "user.name", "bench"], ["add", "-A"], ["commit", "-qm", "base"]):
        subprocess.run(["git", *cmd], cwd=root, check=True, capture_output=True)
    (root / "src" / "module_0.ts").write_text("// changed\n" + (root / "src" / "module_0.ts").read_text())


def script() -> list:
    """20 fresh reads with a search beside each, then the first 8 files and the first
    searches again (what the incident did after its old results were cleared)."""
    turns = []
    for i in range(20):
        turns.append(calls(("read_file", {"path": f"src/module_{i}.ts"}), ("grep", {"pattern": f"value_{i}_1\\b", "path": "src"})))
    for i in range(8):
        turns.append(calls(("read_file", {"path": f"src/module_{i}.ts"}), ("grep", {"pattern": f"value_{i}_1\\b", "path": "src"})))
    for _ in range(4):
        turns.append(calls(("git", {"op": "status"}), ("git", {"op": "diff"})))
    turns.append(text("review done"))
    return turns


def flat(m: dict) -> str:
    return json.dumps(m, sort_keys=True)


def shared_prefix_chars(a: list[dict], b: list[dict]) -> int:
    n = 0
    for x, y in zip(a, b):
        sx, sy = flat(x), flat(y)
        if sx == sy:
            n += len(sx)
            continue
        n += next((k for k, (p, q) in enumerate(zip(sx, sy)) if p != q), min(len(sx), len(sy)))
        break
    return n


class MeteredClient(ScriptedClient):
    """Reports a prompt size and an estimated cache read for every request."""

    async def chat_completion(self, messages, tools=None, stream=True, role="default"):
        prev = self.requests[-1] if self.requests else []
        chars = sum(len(flat(m)) for m in messages)
        cached = shared_prefix_chars(prev, messages) // 4
        self.sizes.append((chars // 4, min(cached, chars // 4)))
        self.requests.append(json.loads(json.dumps(messages)))  # frozen: the history is edited in place later
        usage = TokenUsage(prompt_tokens=chars // 4, completion_tokens=50, total_tokens=chars // 4 + 50, cached_tokens=min(cached, chars // 4))
        for ev in self.turns.pop(0):
            if ev.type == StreamEventType.MESSAGE_COMPLETE:
                ev = StreamEvent(type=ev.type, usage=usage, finish_reason=ev.finish_reason, text_delta=ev.text_delta)
            yield ev


async def replay(compact_at: int, budget: int | None) -> dict:
    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        build_fixture(root)
        cfg = Config(cwd=root, approval=ApprovalPolicy.AUTO, auto_verify=False, auto_review=False, auto_scout=False,
                     check_references=False, compact_at_tokens=compact_at, max_turns=100, trace=False,
                     **({"max_total_tokens": budget} if budget else {}))  # only set here, never in the user's config
        async with Agent(cfg) as agent:
            client = MeteredClient(script())
            client.sizes = []
            agent.session.client = client
            async for _ in agent.run("review the changes in src"):
                pass
            requests = client.requests
            ran = [e for e in agent.session.tracer.events if e["event"] == "tool_call"]
    carried_dupes = 0
    for req in requests:
        seen: set[str] = set()
        for m in req:
            if m["role"] == "tool":
                c = m.get("content", "")
                if c in seen and len(c) > 400:
                    carried_dupes += len(c)
                seen.add(c)
    total = sum(s for s, _ in client.sizes)
    cached = sum(c for _, c in client.sizes)
    return {
        "requests": len(requests),
        "tools_executed": len(ran),
        "tool_result_chars_ingested": sum(e.get("chars", 0) for e in ran),
        "input_tokens_est": total,
        "cached_tokens_est": cached,
        "uncached_tokens_est": total - cached,
        "duplicate_result_chars_carried": carried_dupes,
        "last_request_tokens_est": client.sizes[-1][0],
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--label", required=True)
    ap.add_argument("--compact-at", type=int, default=200_000)
    ap.add_argument("--budget", type=int, default=None, help="replay-only override of max_total_tokens; default keeps the config value")
    ap.add_argument("--out", default=str(Path(__file__).parent / "results"))
    args = ap.parse_args()
    runs = [asyncio.run(replay(args.compact_at, args.budget)) for _ in range(3)]  # deterministic, but cheap to confirm
    keys = runs[0].keys()
    out = {"label": args.label, "compact_at_tokens": args.compact_at, "budget": args.budget or "config default", "runs": runs,
           "median": {k: statistics.median(r[k] for r in runs) for k in keys}}
    Path(args.out).mkdir(parents=True, exist_ok=True)
    (Path(args.out) / f"context-replay-{args.label}.json").write_text(json.dumps(out, indent=1))
    print(json.dumps(out["median"], indent=1))


if __name__ == "__main__":
    main()
