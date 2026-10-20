"""What melon learns about a repo between sessions, kept outside the repo (data dir).

Only things that cannot be hallucinated go in:
  * lessons   - what YOU told it (a rejection with feedback, `/learn ...`). Shown in the
                system prompt, newest first, capped; `/lessons` lists them, `/forget N` removes one.
  * edit history - request words -> the files a verified run changed. `search` boosts files that
                fixed similar requests before, so it finds the right place in fewer calls.
Nothing the model merely claims is stored."""
from __future__ import annotations

import hashlib
import json
import time
from pathlib import Path

from utils.paths import atomic_write
from utils.rank import tokenize

MAX_LESSONS = 30
MAX_EDITS = 200
PROMPT_CHARS = 1500
MIN_OVERLAP = 0.5  # share of the search query's words a past request must have in common
HEADING = "## Learned by melon"
OLD_HEADING = "## Learned by mew"  # what repos learned before the rename still have; renamed on the next write
JUDGE_PROMPT = """You watch a coding agent work with its user. Decide whether the user's NEW message teaches the agent something worth remembering for this repo.

Worth keeping:
- the user redirects or corrects the agent ("no, not that", "I wanted X", changing their mind, giving something other than what the agent did)
- the user confirms an approach was right ("yes exactly like that", "perfect, keep doing it this way")
- the user states a preference or convention, however it is phrased

Not worth keeping: a new unrelated task, a question, a one-off detail that will not matter next time, thanks, small talk.

Reply with ONE short rule in the imperative that would help next time, or nothing at all. Say what to do and, when the correction shows it, what not to do. Example:
  agent built a dark mode toggle, user: "no, follow the system theme instead" -> Dark mode follows the system theme; do not add a manual toggle.
Reply with only the rule, or the single word NONE."""


async def judge(client, before: str, answer: str, message: str) -> str:
    """Ask a model whether `message` (the user's reply) is a correction, a confirmation or a preference worth keeping.
    Returns the rule to save, or "" when there is nothing to learn. Never raises: learning must not hurt a run."""
    from client.response import StreamEventType

    ask = f"The user asked: {' '.join(before.split())[:400]}\nThe agent answered: {' '.join(answer.split())[:600]}\nThe user's NEW message: {' '.join(message.split())[:400]}"
    out = ""
    try:
        async for ev in client.chat_completion(
            [{"role": "system", "content": JUDGE_PROMPT}, {"role": "user", "content": ask}], stream=False, role="summarize",
        ):
            if ev.type == StreamEventType.MESSAGE_COMPLETE and ev.text_delta:
                out += ev.text_delta.content
            elif ev.type == StreamEventType.ERROR:
                return ""
    except Exception:
        return ""
    rule = " ".join(out.split()).strip("\"'` ")
    return "" if not rule or rule.upper().startswith("NONE") or len(rule) > 300 else rule


class Learned:
    def __init__(self, root: Path, base: Path | None = None):
        if base is None:
            from config.loader import get_data_dir

            base = get_data_dir()
        self.root = Path(root).resolve()
        self.path = base / "learned" / (hashlib.sha1(str(self.root).encode()).hexdigest()[:16] + ".json")
        self.data = {"lessons": [], "edits": []}
        try:
            loaded = json.loads(self.path.read_text())
            self.data.update({k: loaded.get(k, []) for k in self.data})
        except (OSError, ValueError):
            pass

    def _save(self) -> None:
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            atomic_write(self.path, json.dumps(self.data, indent=1))
        except OSError:
            pass  # learning is a bonus: never fail a run over it

    # lessons -----------------------------------------------------------------
    def add_lesson(self, text: str, source: str = "user") -> bool:
        text = " ".join(text.split())[:300]
        if len(text.split()) < 4 or any(l["text"].lower() == text.lower() for l in self.data["lessons"]):
            return False
        self.data["lessons"].append({"text": text, "source": source, "ts": int(time.time()), "in_file": self._to_agents_md(text)})
        self.data["lessons"] = self.data["lessons"][-MAX_LESSONS:]
        self._save()
        return True

    def _to_agents_md(self, text: str) -> bool:
        """Also write the lesson into the repo's AGENTS.md under '## Learned by melon', so the team gets it too and the
        next session reads it as an ordinary instruction. The section is melon's own and kept last, so a bullet is appended.
        False when the file can't be written."""
        f = self.root / "AGENTS.md"
        try:
            body = (f.read_text(encoding="utf-8") if f.is_file() else "").replace(OLD_HEADING, HEADING)
            if HEADING not in body:
                body = (body.rstrip("\n") + "\n\n" if body.strip() else "") + HEADING + "\n"
            f.write_text(body.rstrip("\n") + f"\n- {text}\n", encoding="utf-8")
            return True
        except OSError:
            return False

    def _out_of_agents_md(self, text: str) -> None:
        f = self.root / "AGENTS.md"
        try:
            f.write_text(f.read_text(encoding="utf-8").replace(f"- {text}\n", "", 1), encoding="utf-8")
        except OSError:
            pass

    def forget(self, index: int) -> str | None:
        ls = self.data["lessons"]
        if 1 <= index <= len(ls):
            gone = ls.pop(index - 1)
            if gone.get("in_file"):
                self._out_of_agents_md(gone["text"])
            self._save()
            return gone["text"]
        return None

    def lessons(self) -> list[str]:
        return [l["text"] for l in self.data["lessons"]]

    def prompt_section(self) -> str:
        out, used = [], 0
        for l in reversed(self.data["lessons"]):  # newest first, so old ones fall off the cap
            text = l["text"]
            if l.get("in_file"):
                continue  # already in AGENTS.md, which the prompt carries
            if used + len(text) > PROMPT_CHARS:
                break
            out.append(f"- {text}")
            used += len(text)
        if not out:
            return ""
        return "# Learned in this repo\n\nThe user taught you these; follow them:\n" + "\n".join(out)

    # edit history ---------------------------------------------------------------
    def record_run(self, request: str, changed: list[str]) -> None:
        """Call only for runs that ended verified; `changed` are repo-relative paths."""
        terms = list(dict.fromkeys(tokenize(request)))[:40]  # first-said words carry the subject of a long brief
        files = [f for f in changed if (self.root / f).is_file()][:8]
        if len(terms) < 3 or not files or any(e["terms"] == terms and e["files"] == files for e in self.data["edits"]):
            return
        self.data["edits"].append({"terms": terms, "files": files, "ts": int(time.time())})
        self.data["edits"] = self.data["edits"][-MAX_EDITS:]
        self._save()

    def prior(self, query: str) -> dict[str, float]:
        """File -> boost (0..1) from past verified runs whose request resembled this query."""
        q = set(tokenize(query))
        if not q:
            return {}
        boosts: dict[str, float] = {}
        for e in self.data["edits"]:
            t = set(e["terms"])
            # share of the query's words, not of both: a 40-word brief would otherwise drown a 5-word search
            overlap = len(q & t) / len(q)
            if overlap >= MIN_OVERLAP:
                for f in e["files"]:
                    boosts[f] = min(1.0, boosts.get(f, 0.0) + overlap)
        return boosts
