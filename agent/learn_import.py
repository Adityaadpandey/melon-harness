"""Teach mew from the Claude Code sessions you already ran in a repo.

Claude Code keeps every session as a jsonl file under ~/.claude/projects/<cwd with / as ->/.
Each message you typed starts a task, and the task runs until your next message. Every task gets a reward:

  -1  you interrupted it, rejected one of its tool calls, or your next message corrects it ("no, ...", "still broken")
  +1  your next message moves on ("commit it", "works", "nice")
   0  anything else

Tasks that were not punished go into mew's edit history (request words -> files Claude edited), so `search`
finds those files faster next time. The rules you stated along the way ("commit in parts pls") become lessons.
They land in mew's store and in the repo's AGENTS.md. Picking rules out of casual chat needs a model, so that
step is one model call. Hook blocks that repeated ("Biome lint has warnings ... fix them before pushing") come
along as evidence too.

    python -m agent.learn_import --repo ~/Devlopment/tunyt            # dry run: prints what it would learn
    python -m agent.learn_import --repo ~/Devlopment/tunyt --apply    # writes it
"""
from __future__ import annotations

import argparse
import asyncio
import json
import re
from collections import Counter
from pathlib import Path

from agent.learn import Learned

EDIT_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit"}
INTERRUPTED = "[Request interrupted by user"
# a reply that opens like this means the last task went wrong: "no that's not it", "still broken", "revert that"
CORRECTION = re.compile(r"^\W*(no\b|nope|wrong|revert|undo|that'?s not|not working|doesn'?t work|still (broken|not|fails|failing|the same)|it broke|you broke|why did you|stop\b)", re.I)
# ...and this means it went fine
APPROVAL = re.compile(r"^\W*(ok\w*|okay|nice|great|perfect|works|it works|good|cool|thanks|done|commit|push|ship)\b", re.I)
NOT_TYPED = ("<", "This session is being continued", "Caveat:")  # slash commands, compaction summaries, hook notes
PASTE = re.compile(r"<pasted_content[^>]*>.*?</pasted_content>", re.S)
MAX_NEW_LESSONS = 15


def project_dirs(repo: Path, projects: Path) -> list[Path]:
    """Claude Code folders for the repo and its subfolders: /a/tunyt -> -a-tunyt, -a-tunyt-apps-web."""
    key = str(repo).replace("/", "-")
    return sorted(d for d in projects.glob(key + "*") if d.is_dir() and (d.name == key or d.name.startswith(key + "-")))


def _texts(content) -> list[str]:
    if isinstance(content, str):
        return [content]
    if isinstance(content, list):
        return [b.get("text", "") for b in content if isinstance(b, dict) and b.get("type") == "text"]
    return []


def episodes(lines, repo: Path) -> tuple[list[dict], Counter]:
    """One session's records -> tasks [{request, files, neg}] plus how often each hook block fired."""
    tasks: list[dict] = []
    hooks: Counter = Counter()
    for line in lines:
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if r.get("isSidechain") or r.get("isMeta"):
            continue  # subagent chatter and injected notes are not you talking
        content = (r.get("message") or {}).get("content")
        if r.get("type") == "user":
            for b in content if isinstance(content, list) else []:
                if isinstance(b, dict) and b.get("type") == "tool_result" and tasks:
                    if r.get("toolDenialKind") == "user-rejected":
                        tasks[-1]["neg"] = True
                    text = b.get("content") if isinstance(b.get("content"), str) else ""
                    if "hook error:" in text:
                        hooks[text.split("hook error:", 1)[1].strip()[:200]] += 1
            for t in _texts(content):
                if t.startswith(INTERRUPTED):
                    if tasks:
                        tasks[-1]["neg"] = True
                elif t.strip() and not t.lstrip().startswith(NOT_TYPED):
                    tasks.append({"request": PASTE.sub(" ", t).strip(), "files": [], "neg": False})
        elif r.get("type") == "assistant" and isinstance(content, list) and tasks:
            for b in content:
                if isinstance(b, dict) and b.get("type") == "tool_use" and b.get("name") in EDIT_TOOLS:
                    path = (b.get("input") or {}).get("file_path") or (b.get("input") or {}).get("notebook_path") or ""
                    try:
                        rel = str(Path(path).resolve().relative_to(repo))
                    except ValueError:
                        continue  # a scratch file or another repo
                    if rel not in tasks[-1]["files"]:
                        tasks[-1]["files"].append(rel)
    # your next message is the verdict on the task before it
    for task, nxt in zip(tasks, tasks[1:] + [None]):
        reply = nxt["request"] if nxt else ""
        task["reward"] = -1 if task["neg"] or CORRECTION.match(reply) else 1 if APPROVAL.match(reply) else 0
    return tasks, hooks


def collect(repo: Path, projects: Path) -> tuple[list[dict], Counter]:
    tasks, hooks = [], Counter()
    for d in project_dirs(repo, projects):
        for f in sorted(d.glob("*.jsonl"), key=lambda p: p.stat().st_mtime):  # oldest first, like they happened
            t, h = episodes(f.read_text(errors="replace").splitlines(), repo)
            tasks += t
            hooks += h
    return tasks, hooks


LESSON_PROMPT = """You read what a developer typed to a coding agent while working in one repository, oldest first.
Pull out the STANDING rules: preferences that should hold for every future task in this repo, such as how to
commit, which tools or commands to use, what never to touch, or how they want work checked.
Skip one-off task instructions ("fix the login bug", "add a refund button") and anything only true for that day.
A rule they repeated, or corrected the agent about, is the strongest evidence. "Hook blocks" lists repo hooks
that stopped the agent, with how often each fired.
Write each rule as one short imperative line (max 25 words) in plain English. Fix their typos.
Leave out anything already in "Already known". Return at most {n} rules as a JSON array of strings, nothing else."""


def evidence(tasks: list[dict], hooks: Counter, known: list[str]) -> str:
    """The text the model reads. Long briefs and pasted docs are cut short, since rules are said in a line."""
    said = list(dict.fromkeys(" ".join(t["request"].split())[:300] for t in tasks if t["request"]))
    corrected = [" ".join(n["request"].split())[:300] for t, n in zip(tasks, tasks[1:]) if t["reward"] < 0]
    parts = ["Messages:\n" + "\n".join(f"- {m}" for m in said)]
    if corrected:
        parts.append("Replies right after the agent got it wrong:\n" + "\n".join(f"- {m}" for m in corrected))
    if hooks:
        parts.append("Hook blocks:\n" + "\n".join(f"- ({n}x) {h}" for h, n in hooks.most_common(10)))
    if known:
        parts.append("Already known:\n" + "\n".join(f"- {k}" for k in known))
    return "\n\n".join(parts)


async def distill(text: str, cwd: Path) -> list[str]:
    from client.llm_client import LLMClient
    from client.response import StreamEventType
    from config.loader import load_config

    client = LLMClient(load_config(cwd))
    await client.initialize()
    out = ""
    try:
        async for ev in client.chat_completion(
            [{"role": "system", "content": LESSON_PROMPT.format(n=MAX_NEW_LESSONS)}, {"role": "user", "content": text}],
            stream=False, role="plan",
        ):
            if ev.type == StreamEventType.MESSAGE_COMPLETE and ev.text_delta:
                out += ev.text_delta.content
            elif ev.type == StreamEventType.ERROR:
                raise SystemExit(f"model call failed: {ev.error}")
    finally:
        await client.close()
    try:
        rules = json.loads(out[out.index("["): out.rindex("]") + 1])
    except ValueError:
        raise SystemExit(f"model did not return a JSON list:\n{out[:500]}")
    return [r for r in rules if isinstance(r, str)][:MAX_NEW_LESSONS]


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--repo", type=Path, required=True)
    ap.add_argument("--projects", type=Path, default=Path.home() / ".claude" / "projects")
    ap.add_argument("--apply", action="store_true", help="write to mew's store and AGENTS.md (default: dry run)")
    ap.add_argument("--no-lessons", action="store_true", help="edit history only, skip the model call")
    args = ap.parse_args(argv)
    repo = args.repo.expanduser().resolve()

    tasks, hooks = collect(repo, args.projects)
    rewards = Counter(t["reward"] for t in tasks)
    kept = [t for t in tasks if t["reward"] >= 0 and t["files"]]
    print(f"{len(tasks)} tasks: {rewards[1]} rewarded, {rewards[0]} neutral, {rewards[-1]} punished; "
          f"{len(kept)} unpunished tasks edited files")

    learned = Learned(repo)
    before = len(learned.data["edits"])
    if args.apply:
        for t in kept:
            learned.record_run(t["request"], t["files"])  # drops files that are gone and keeps the newest 200
        print(f"edit history: {before} -> {len(learned.data['edits'])}")

    if args.no_lessons:
        return
    text = evidence(tasks, hooks, learned.lessons())
    print(f"asking a model for lessons ({len(text) // 4} tokens of evidence)")
    rules = asyncio.run(distill(text, repo))
    for r in rules:
        if args.apply:
            print(("+ " if learned.add_lesson(r, "claude-code") else "= ") + r)
        else:
            print(f"? {r}")
    if not args.apply:
        print("dry run: nothing written. Add --apply to save these lessons and the edit history.")


if __name__ == "__main__":
    main()
