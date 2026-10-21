"""Teach melon from the Claude Code sessions you already ran in a repo.

Claude Code keeps every session as a jsonl file under ~/.claude/projects/<cwd with / as ->/, but it deletes
them after 30 days unless `cleanupPeriodDays` is raised. What it never deletes is ~/.claude/history.jsonl:
every prompt you typed, with its folder and session id. So there are two sources:

  * full transcripts (recent): your prompts, the files Claude edited, the commands that failed, what it said
  * prompt history (all time): only your prompts, but still in order, so a "no, ..." still punishes the one before

Each message you typed starts a task, and the task runs until your next message. Every task gets a reward:

  -1  you interrupted it, rejected one of its tool calls, or your next message corrects it ("no, ...", "still broken")
  +1  your next message moves on ("commit it", "works", "nice")
   0  anything else

Tasks that were not punished go into melon's edit history (request words -> files Claude edited), so `search`
finds those files faster next time. The rules you stated along the way ("commit in parts pls") become lessons,
saved in melon's store and in the repo's AGENTS.md.

The quick run asks one model for lessons from your recent prompts. `--deep` reads everything: each task's story
goes to a model in chunks for candidate lessons, then one last call weighs them against what the repo already
says (AGENTS.md files, Command Code taste, skills) so only new rules are added, and it also drops lessons melon
saved before that turned out to be noise or repeat the rulebook.

    python -m agent.learn_import --repo ~/Devlopment/tunyt                   # quick dry run
    python -m agent.learn_import --repo ~/Devlopment/tunyt --deep            # deep dry run: prints adds and drops
    python -m agent.learn_import --repo ~/Devlopment/tunyt --deep --reweigh  # same candidates, new verdict (cheap)
    python -m agent.learn_import --repo ~/Devlopment/tunyt --deep --apply    # writes the verdict you just reviewed
"""
from __future__ import annotations

import argparse
import asyncio
import json
import re
from collections import Counter
from pathlib import Path

from agent.learn import HEADING, MAX_LESSONS, Learned

EDIT_TOOLS = {"Edit", "Write", "MultiEdit", "NotebookEdit"}
INTERRUPTED = "[Request interrupted by user"
# a reply that opens like this means the last task went wrong: "no that's not it", "still broken", "revert that"
CORRECTION = re.compile(r"^\W*(no\b|nope|wrong|revert|undo|that'?s not|not working|doesn'?t work|still (broken|not|fails|failing|the same)|it broke|you broke|why did you|stop\b)", re.I)
# ...and this means it went fine
APPROVAL = re.compile(r"^\W*(ok\w*|okay|nice|great|perfect|works|it works|good|cool|thanks|done|commit|push|ship)\b", re.I)
NOT_TYPED = ("<", "/", "This session is being continued", "Caveat:")  # slash commands, compaction summaries, hook notes
PASTE = re.compile(r"<pasted_content[^>]*>.*?</pasted_content>|\[Pasted text #\d+[^\]]*\]", re.S)
MAX_NEW_LESSONS = 15
CHUNK_CHARS = 80_000  # about 20k tokens of task stories per model call
PARALLEL = 3


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


def _task(request: str) -> dict:
    return {"request": PASTE.sub(" ", request).strip(), "files": [], "neg": False, "said": "", "failed": []}


def _score(tasks: list[dict]) -> list[dict]:
    """Your next message is the verdict on the task before it."""
    for task, nxt in zip(tasks, tasks[1:] + [None]):
        task["reply"] = nxt["request"] if nxt else ""
        task["reward"] = -1 if task["neg"] or CORRECTION.match(task["reply"]) else 1 if APPROVAL.match(task["reply"]) else 0
    return tasks


def episodes(lines, repo: Path) -> tuple[list[dict], Counter]:
    """One session's records -> tasks plus how often each hook block fired. A task looks like
    {request, files, neg, said (Claude's last words), failed (commands that errored), reply, reward}."""
    tasks: list[dict] = []
    hooks: Counter = Counter()
    commands: dict[str, str] = {}  # tool_use id -> the shell command, so a failed result can name it
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
                    elif b.get("is_error") and b.get("tool_use_id") in commands:
                        tasks[-1]["failed"].append(f"{commands[b['tool_use_id']][:120]} -> {' '.join(text.split())[:160]}")
            for t in _texts(content):
                if t.startswith(INTERRUPTED):
                    if tasks:
                        tasks[-1]["neg"] = True
                elif t.strip() and not t.lstrip().startswith(NOT_TYPED):
                    tasks.append(_task(t))
        elif r.get("type") == "assistant" and isinstance(content, list) and tasks:
            for b in content:
                if not isinstance(b, dict):
                    continue
                if b.get("type") == "text" and b.get("text", "").strip():
                    tasks[-1]["said"] = b["text"]
                if b.get("type") != "tool_use":
                    continue
                args = b.get("input") or {}
                if b.get("name") == "Bash":
                    commands[b.get("id", "")] = " ".join(str(args.get("command", "")).split())
                if b.get("name") in EDIT_TOOLS:
                    path = args.get("file_path") or args.get("notebook_path") or ""
                    try:
                        rel = str(Path(path).resolve().relative_to(repo))
                    except ValueError:
                        continue  # a scratch file or another repo
                    if rel not in tasks[-1]["files"]:
                        tasks[-1]["files"].append(rel)
    return _score(tasks), hooks


def collect(repo: Path, projects: Path) -> tuple[list[dict], Counter, set[str]]:
    """Every task from the transcripts still on disk, the hook counts, and which session ids those were."""
    tasks, hooks, seen = [], Counter(), set()
    for d in project_dirs(repo, projects):
        for f in sorted(d.glob("*.jsonl"), key=lambda p: p.stat().st_mtime):  # oldest first, like they happened
            t, h = episodes(f.read_text(errors="replace").splitlines(), repo)
            tasks += t
            hooks += h
            seen.add(f.stem)
    return tasks, hooks, seen


def history_tasks(repo: Path, history: Path, skip: set[str]) -> list[dict]:
    """Tasks from the prompt history, for sessions whose transcript Claude Code already deleted.
    Only the prompts are left, so these tasks have no files and no story, just the request and its verdict."""
    sessions: dict[str, list[tuple[int, str]]] = {}
    try:
        lines = history.read_text(errors="replace").splitlines()
    except OSError:
        return []
    for line in lines:
        try:
            r = json.loads(line)
        except ValueError:
            continue
        project, sid, text = r.get("project", ""), r.get("sessionId", ""), r.get("display", "")
        if sid in skip or not text.strip() or text.lstrip().startswith(NOT_TYPED):
            continue
        if project == str(repo) or project.startswith(str(repo) + "/"):
            sessions.setdefault(sid, []).append((r.get("timestamp", 0), text))
    tasks = []
    for prompts in sorted(sessions.values(), key=lambda p: p[0][0]):
        tasks += _score([_task(text) for _, text in sorted(prompts)])
    return tasks


def _one_line(text: str, n: int) -> str:
    return " ".join(text.split())[:n]


def story(t: dict) -> str:
    """One task as a few lines a model can read. Punished tasks get more room, since that's where the lessons are."""
    room = 900 if t["reward"] < 0 else 500
    out = [f"[{'+1' if t['reward'] > 0 else t['reward']}] YOU: {_one_line(t['request'], room)}"]
    if t["files"]:
        out.append("  edited: " + ", ".join(t["files"][:6]))
    for f in t["failed"][:3]:
        out.append(f"  failed: {f}")
    if t["said"]:
        out.append(f"  CLAUDE: {_one_line(t['said'], 400 if t['reward'] < 0 else 200)}")
    if t["reward"] < 0 and t["reply"]:
        out.append(f"  YOU NEXT: {_one_line(t['reply'], 400)}")
    return "\n".join(out)


def chunks(tasks: list[dict], size: int = CHUNK_CHARS) -> list[str]:
    out, cur = [], ""
    for t in tasks:
        if not t["request"]:
            continue
        s = story(t) + "\n"
        if cur and len(cur) + len(s) > size:
            out.append(cur)
            cur = ""
        cur += s
    return out + ([cur] if cur else [])


def rulebook(repo: Path) -> str:
    """What the repo already tells agents: every AGENTS.md (minus melon's own section, which is under review),
    Command Code taste, and each skill's name and description."""
    parts = []
    for f in sorted(repo.rglob("AGENTS.md")):
        if "node_modules" in f.parts or ".deepsec" in f.parts:
            continue
        body = f.read_text(errors="replace").split(HEADING)[0]
        parts.append(f"=== {f.relative_to(repo)}\n{body.strip()}")
    for f in sorted((repo / ".commandcode" / "taste").rglob("*.md")):
        parts.append(f"=== {f.relative_to(repo)}\n{f.read_text(errors='replace').strip()}")
    skills = []
    for f in sorted(repo.glob(".claude/skills/*/SKILL.md")) + sorted(repo.glob("docs/skills/*.md")):
        desc = re.search(r"^description:\s*(.+)$", f.read_text(errors="replace"), re.M)
        skills.append(f"- {f.relative_to(repo)}: {desc.group(1).strip()[:200] if desc else ''}")
    if skills:
        parts.append("=== skills\n" + "\n".join(skills))
    return "\n\n".join(parts)


async def ask(system: str, user: str, cwd: Path, client=None) -> str:
    from client.llm_client import LLMClient
    from client.response import StreamEventType
    from config.loader import load_config

    own = client is None
    if own:
        client = LLMClient(load_config(cwd))
        await client.initialize()
    out = ""
    try:
        async for ev in client.chat_completion(
            [{"role": "system", "content": system}, {"role": "user", "content": user}], stream=False, role="plan",
        ):
            if ev.type == StreamEventType.MESSAGE_COMPLETE and ev.text_delta:
                out += ev.text_delta.content
            elif ev.type == StreamEventType.ERROR:
                raise SystemExit(f"model call failed: {ev.error}")
    finally:
        if own:
            await client.close()
    return out


def _json(out: str, open_: str, close: str):
    try:
        return json.loads(out[out.index(open_): out.rindex(close) + 1])
    except ValueError:
        raise SystemExit(f"model did not return JSON:\n{out[:500]}")


# quick mode ------------------------------------------------------------------------------------------------

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
    said = list(dict.fromkeys(_one_line(t["request"], 300) for t in tasks if t["request"]))
    corrected = [_one_line(t["reply"], 300) for t in tasks if t["reward"] < 0 and t["reply"]]
    parts = ["Messages:\n" + "\n".join(f"- {m}" for m in said)]
    if corrected:
        parts.append("Replies right after the agent got it wrong:\n" + "\n".join(f"- {m}" for m in corrected))
    if hooks:
        parts.append("Hook blocks:\n" + "\n".join(f"- ({n}x) {h}" for h, n in hooks.most_common(10)))
    if known:
        parts.append("Already known:\n" + "\n".join(f"- {k}" for k in known))
    return "\n\n".join(parts)


# deep mode -------------------------------------------------------------------------------------------------

MAP_PROMPT = """You read stories of tasks a developer gave a coding agent in one repository, oldest first.
Each starts with the reward: [-1] the developer interrupted, rejected or corrected the agent, [+1] they moved on
happily, [0] no signal. YOU is the developer, CLAUDE is the agent's last words, "failed" are commands that errored,
YOU NEXT is the developer's reply after a punished task.
Find what an agent should know before its next task here:
  * standing preferences the developer stated or kept enforcing (how to commit, what to ask first, scope, tone)
  * mistakes the agent made more than once, or that the developer corrected sharply, and what to do instead
  * repo gotchas: a command that fails and the one that works, a file that is the real source of truth
Skip one-off feature requests and anything only true that day. Be concrete: name commands, files, paths.
Return a JSON array of objects {{"rule": "<one imperative line, max 30 words>", "evidence": "<short quote or count>"}}.
At most {n} items, the best supported first. Nothing else."""

REDUCE_PROMPT = """You maintain the "Learned by melon" section of a repository's AGENTS.md: rules an agent learned
from working with this developer. You get:
  * RULEBOOK: what the repo already tells agents (AGENTS.md files, learned taste, skills)
  * CURRENT: the numbered lessons in the section now
  * CANDIDATES: rules mined from hundreds of the developer's past sessions, with evidence
Decide the final section:
  * drop a CURRENT lesson that the RULEBOOK already says (even in other words, and above all anything the
    Absolute Rules cover), that is vague, that is a fragment or an error message rather than a rule, or that a
    better candidate replaces. Keeping a duplicate of the rulebook is a mistake: the agent reads both.
  * add a CANDIDATE only if the RULEBOOK does not already say it, it is supported by real evidence, and it will
    still be true next month. Merge candidates that say the same thing. Prefer rules with concrete commands/paths.
  * a lesson is about how an agent should WORK in this repo (asking, scope, verifying, committing, commands,
    gotchas). How a product feature should behave ("the event AI asks one question at a time") is a product
    requirement for that feature's spec, not a lesson: leave it out.
  * never add anything that contradicts the RULEBOOK
The section may hold at most {cap} lessons after your changes. Each new lesson is one plain English line, max 30 words.
Return JSON {{"drop": [<CURRENT numbers>], "add": ["<lesson>", ...], "why": {{"<lesson or number>": "<few words>"}}}}.
Nothing else."""


async def deep(tasks: list[dict], hooks: Counter, repo: Path, learned: Learned, found: list[dict] | None = None) -> dict:
    """Mine candidates from every task (skipped when `found` is handed in from a saved run), then weigh them."""
    from client.llm_client import LLMClient
    from config.loader import load_config

    parts = chunks(tasks)
    if hooks:
        parts.append("Hook blocks that stopped the agent:\n" + "\n".join(f"- ({n}x) {h}" for h, n in hooks.most_common(10)))
    client = LLMClient(load_config(repo))
    await client.initialize()
    gate = asyncio.Semaphore(PARALLEL)

    async def mine(i: int, text: str) -> list[dict]:
        async with gate:
            got = _json(await ask(MAP_PROMPT.format(n=12), text, repo, client), "[", "]")
            print(f"  chunk {i + 1}/{len(parts)}: {len(got)} candidates")
            return [c for c in got if isinstance(c, dict) and c.get("rule")]

    try:
        if found is None:
            print(f"mining {len(parts)} chunks ({sum(map(len, parts)) // 4} tokens)")
            found = [c for got in await asyncio.gather(*(mine(i, p) for i, p in enumerate(parts))) for c in got]
        current = learned.lessons()
        user = "\n\n".join([
            "RULEBOOK:\n" + rulebook(repo),
            "CURRENT:\n" + "\n".join(f"{i}. {t}" for i, t in enumerate(current, 1)),
            "CANDIDATES:\n" + "\n".join(f"- {c['rule']} (evidence: {_one_line(str(c.get('evidence', '')), 160)})" for c in found),
        ])
        print(f"weighing {len(found)} candidates against the rulebook ({len(user) // 4} tokens)")
        verdict = _json(await ask(REDUCE_PROMPT.format(cap=MAX_LESSONS), user, repo, client), "{", "}")
    finally:
        await client.close()
    drop = sorted({n for n in verdict.get("drop", []) if isinstance(n, int) and 1 <= n <= len(current)})
    room = MAX_LESSONS - (len(current) - len(drop))
    add = [a for a in verdict.get("add", []) if isinstance(a, str)][:max(room, 0)]
    return {"drop": [current[n - 1] for n in drop], "add": add, "why": verdict.get("why", {}), "candidates": found}


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--repo", type=Path, required=True)
    ap.add_argument("--projects", type=Path, default=Path.home() / ".claude" / "projects")
    ap.add_argument("--history", type=Path, default=Path.home() / ".claude" / "history.jsonl")
    ap.add_argument("--apply", action="store_true", help="write to melon's store and AGENTS.md (default: dry run)")
    ap.add_argument("--no-lessons", action="store_true", help="edit history only, skip the model calls")
    ap.add_argument("--deep", action="store_true", help="read every session's story and check lessons against the rulebook")
    ap.add_argument("--reweigh", action="store_true", help="with --deep: reuse the saved candidates, redo only the final verdict")
    args = ap.parse_args(argv)
    repo = args.repo.expanduser().resolve()

    tasks, hooks, seen = collect(repo, args.projects)
    old = history_tasks(repo, args.history, seen) if args.deep else []
    rewards = Counter(t["reward"] for t in tasks + old)
    kept = [t for t in tasks if t["reward"] >= 0 and t["files"]]
    print(f"{len(seen)} transcripts ({len(tasks)} tasks) + {len(old)} prompts from "
          f"deleted sessions: {rewards[1]} rewarded, {rewards[0]} neutral, {rewards[-1]} punished; "
          f"{len(kept)} unpunished tasks edited files")

    learned = Learned(repo)
    if args.apply:
        before = len(learned.data["edits"])
        for t in kept:
            learned.record_run(t["request"], t["files"])  # drops files that are gone, skips repeats, keeps the newest 200
        print(f"edit history: {before} -> {len(learned.data['edits'])}")
    if args.no_lessons:
        return

    if args.deep:
        # the dry run is saved next to melon's store, so --apply writes exactly what you reviewed (no new model call)
        # and --reweigh redoes only the last, cheap step on the same candidates
        saved_path = learned.path.with_suffix(".deep.json")
        try:
            saved = json.loads(saved_path.read_text())
        except (OSError, ValueError):
            saved = None
        if args.apply and saved and not args.reweigh:
            result = saved
            print(f"applying the dry run saved in {saved_path}")
        else:
            reuse = saved["candidates"] if args.reweigh and saved else None
            result = asyncio.run(deep(old + tasks, hooks, repo, learned, reuse))  # oldest first
            saved_path.write_text(json.dumps(result, indent=1))
        for text in result["drop"]:
            print(f"- {text[:160]}")
        for text in result["add"]:
            print(f"+ {text}")
        if args.apply:
            for text in result["drop"]:
                if text in learned.lessons():
                    learned.forget(learned.lessons().index(text) + 1)
            added = sum(learned.add_lesson(text, "claude-code") for text in result["add"])
            saved_path.unlink(missing_ok=True)  # used up: the next run looks at the lessons fresh
            print(f"dropped {len(result['drop'])}, added {added}: {len(learned.lessons())} lessons now")
        else:
            print(f"{len(result['candidates'])} candidates and this verdict saved to {saved_path}")
    else:
        rules = _json(asyncio.run(ask(LESSON_PROMPT.format(n=MAX_NEW_LESSONS), evidence(tasks, hooks, learned.lessons()), repo)), "[", "]")
        for r in [r for r in rules if isinstance(r, str)][:MAX_NEW_LESSONS]:
            print((("+ " if learned.add_lesson(r, "claude-code") else "= ") if args.apply else "? ") + r)
    if not args.apply:
        print("dry run: nothing written. Add --apply to save it.")


if __name__ == "__main__":
    main()
