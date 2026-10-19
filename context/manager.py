from datetime import datetime
import json
from pathlib import Path
from typing import Any
from client.response import TokenUsage
from config.config import Config
from prompts.system import get_system_prompt
from dataclasses import dataclass, field

from tools.base import Tool
from utils.text import count_tokens


@dataclass
class MessageItem:
    role: str
    content: str
    tool_call_id: str | None = None
    tool_calls: list[dict[str, Any]] = field(default_factory=list)
    token_count: int | None = None
    pruned_at: datetime | None = None

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {"role": self.role}

        if self.tool_call_id:
            result["tool_call_id"] = self.tool_call_id

        if self.tool_calls:
            result["tool_calls"] = self.tool_calls

        # providers reject a tool message with no content (e.g. `touch x` prints
        # nothing), so those always carry text; assistant tool-call turns send ""
        if self.content:
            result["content"] = self.content
        elif self.role == "tool":
            result["content"] = "(no output)"
        elif self.role == "assistant":
            result["content"] = ""

        return result


class ContextManager:
    PRUNE_PROTECT_TOKENS = 20_000  # newest tool results kept whole
    PRUNE_MINIMUM_TOKENS = 20_000

    def __init__(
        self,
        config: Config,
        user_memory: str | None,
        tools: list[Tool] | None,
        skills: list | None = None,
    ) -> None:
        self._system_prompt = get_system_prompt(config, user_memory, tools, skills)
        self.config = config
        self._model_name = self.config.model_name
        self._messages: list[MessageItem] = []
        self._latest_usage = TokenUsage()
        self.total_usage = TokenUsage()
        self.context_window = config.model.context_window
        self.summary: str | None = None  # running summary of everything compacted so far
        self.files_read: set[str] = set()
        self.files_modified: set[str] = set()
        self.archive = None  # ResultStore: where a cleared result's full text goes
        self.last_prune: dict = {}

    @property
    def message_count(self) -> int:
        return len(self._messages)

    COMPACT_HEAD = "# Earlier conversation (compacted)"

    def _restore_summary(self, content: str) -> None:
        """A resumed session replays its messages, so the compacted head arrives as plain text.
        Take the summary and the file lists back out of it, otherwise the next compaction would
        treat it as an ordinary (truncated) message and forget which files were edited."""
        self.summary = content.split("\n\n", 2)[2] if content.count("\n\n") >= 2 else content
        for line in self.summary.splitlines():
            for prefix, target in (("Files modified so far: ", self.files_modified), ("Files read so far: ", self.files_read)):
                if line.startswith(prefix):
                    target.update(p.strip() for p in line[len(prefix):].split(","))

    def add_user_message(self, content: str) -> None:
        if not self._messages and content.startswith(self.COMPACT_HEAD):
            self._restore_summary(content)
        item = MessageItem(
            role="user",
            content=content,
            token_count=count_tokens(
                content,
                self._model_name,
            ),
        )

        self._messages.append(item)

    def add_assistant_message(
        self,
        content: str,
        tool_calls: list[dict[str, any]] | None = None,
    ) -> None:
        item = MessageItem(
            role="assistant",
            content=content or "",
            token_count=count_tokens(
                content or "",
                self._model_name,
            ),
            tool_calls=tool_calls or [],
        )

        self._messages.append(item)

    def add_tool_result(self, tool_call_id: str, content: str) -> None:
        item = MessageItem(
            role="tool",
            content=content,
            tool_call_id=tool_call_id,
            token_count=count_tokens(content, self._model_name),
        )

        self._messages.append(item)

    def get_messages(self) -> list[dict[str, Any]]:
        messages = []

        if self._system_prompt:
            messages.append(
                {
                    "role": "system",
                    "content": self._system_prompt,
                }
            )

        for item in self._messages:
            messages.append(item.to_dict())

        return messages

    def _limit(self) -> float:
        if self.config.pi_mode:
            return self.context_window - 16_384  # pi: compact only when the next reply would no longer fit
        # a 1M window doesn't mean 800k tokens of history is a good idea:
        # quality drops and every turn resends it, so cap with compact_at_tokens
        return min(self.context_window * 0.8, self.config.compact_at_tokens)

    def needs_compression(self) -> bool:
        return self._latest_usage.total_tokens > self._limit()

    def set_latest_usage(self, usage: TokenUsage):
        self._latest_usage = usage

    def add_usage(self, usage: TokenUsage):
        self.total_usage += usage

    def find_cut(self, keep_tokens: int) -> int:
        """Index in self._messages where the kept tail starts. The tail holds
        roughly `keep_tokens` of the most recent messages and never starts on a
        tool result (its assistant tool_calls message must travel with it)."""
        total, cut = 0, len(self._messages)
        for i in range(len(self._messages) - 1, -1, -1):
            total += self._messages[i].token_count or 0
            cut = i
            if total >= keep_tokens:
                break
        while cut > 0 and cut < len(self._messages) and self._messages[cut].role == "tool":
            cut -= 1
        return cut

    def history_for_summary(self, cut: int) -> list[dict[str, Any]]:
        # the compacted head is passed whole as the previous summary, not as a message to fold in
        skip = 1 if self.summary and self._messages and self._messages[0].content.startswith(self.COMPACT_HEAD) else 0
        return [m.to_dict() for m in self._messages[skip:cut]]

    def _track_files(self, items: list[MessageItem]) -> None:
        for m in items:
            for tc in m.tool_calls:
                try:
                    args = json.loads(tc["function"]["arguments"])
                except (ValueError, KeyError, TypeError):
                    continue
                path = args.get("path") if isinstance(args, dict) else None
                if not isinstance(path, str):
                    continue
                name = tc["function"]["name"]
                if name == "read_file":
                    self.files_read.add(path)
                elif name in {"edit", "write_file"}:
                    self.files_modified.add(path)

    def recent_files(self, cut: int, limit: int = 5) -> list[str]:
        """Files read or edited in the part about to be compacted, newest first, skipping any the kept tail
        touches again (their text is still there). Claude Code puts these back after a compaction so the
        model does not re-read the five files it was just working in."""
        def paths(items: list[MessageItem]) -> list[str]:
            out = []
            for m in items:
                for tc in m.tool_calls:
                    if tc["function"]["name"] not in {"read_file", "edit", "write_file"}:
                        continue
                    try:
                        path = json.loads(tc["function"]["arguments"]).get("path")
                    except (ValueError, KeyError, TypeError, AttributeError):
                        continue
                    if isinstance(path, str):
                        out.append(path)
            return out

        in_tail = set(paths(self._messages[cut:]))
        picked: list[str] = []
        for path in reversed(paths(self._messages[:cut])):
            if path not in in_tail and path not in picked:
                picked.append(path)
        return picked[:limit]

    def apply_compaction(self, summary: str, cut: int, transcript: Path | None = None) -> None:
        """Replace messages[:cut] with the summary and keep the recent tail verbatim,
        so the model still sees exactly what it just did instead of a retelling.
        With `transcript`, the replaced messages are appended there first (one JSON per line), so nothing
        is gone for good: the summary names the file and the model can grep it for an exact error or diff."""
        self._track_files(self._messages[:cut])
        saved = ""
        if transcript is not None:
            try:
                transcript.parent.mkdir(parents=True, exist_ok=True)
                with transcript.open("a") as fp:
                    for m in self.history_for_summary(cut):
                        fp.write(json.dumps(m) + "\n")
                saved = (f" If you need exact details from before compaction (a code snippet, an error, a list the "
                         f"user pasted), grep or read the full transcript at {transcript}.")
            except OSError:
                pass  # a full disk must never fail a compaction
        files = ""
        if self.files_modified:
            files += f"\n\nFiles modified so far: {', '.join(sorted(self.files_modified))}"
        reads = sorted(self.files_read - self.files_modified)
        if reads:
            files += f"\nFiles read so far: {', '.join(reads[:40])}"
        self.summary = summary + files
        try:
            from agent.notes import read as read_notes

            if notes := read_notes(self.config.cwd):
                files += f"\n\nTask notes (kept with the notes tool, still current):\n{notes[:1500]}"
                self.summary = summary + files
        except Exception:
            pass  # a notes problem must never fail a compaction
        content = (
            f"{self.COMPACT_HEAD}\n\n"
            "Older messages were replaced by this summary to save context. Actions listed as "
            f"completed are done; do not repeat them. The most recent messages follow unchanged.{saved}\n\n"
            f"{self.summary}"
        )
        head = MessageItem(role="user", content=content, token_count=count_tokens(content, self._model_name))
        self._messages = [head] + self._messages[cut:]

    def has_live_result(self, call_id: str) -> bool:
        """True while that call's result is still in the history with its full text."""
        return any(m.tool_call_id == call_id and not m.pruned_at for m in self._messages if m.role == "tool")

    def repair_orphans(self) -> int:
        """After an interrupt, an assistant message may have tool_calls with no
        results. Providers reject that, so close each one with a stub result."""
        fixed, out, i = 0, [], 0
        msgs = self._messages
        while i < len(msgs):
            m = msgs[i]
            out.append(m)
            i += 1
            if m.role == "assistant" and m.tool_calls:
                answered = set()
                while i < len(msgs) and msgs[i].role == "tool":
                    answered.add(msgs[i].tool_call_id)
                    out.append(msgs[i])
                    i += 1
                for tc in m.tool_calls:
                    if tc["id"] not in answered:
                        out.append(MessageItem(role="tool", content="[interrupted before this tool ran]", tool_call_id=tc["id"], token_count=8))
                        fixed += 1
        self._messages = out
        return fixed

    def prune_tool_outputs(self) -> int:
        # Clearing rewrites old messages, which costs one cache re-read of everything after them. It still
        # pays: prompts stay small, so more calls fit the token budget. What hurts is losing a result the
        # model then needs again, so cleared results are archived and can be recalled exactly.
        total_tokens = 0
        pruned_tokens = 0
        to_prune: list[MessageItem] = []

        for msg in reversed(self._messages):
            if msg.role == "tool" and msg.tool_call_id:
                if msg.pruned_at:
                    break

                tokens = msg.token_count or count_tokens(msg.content, self._model_name)
                total_tokens += tokens

                if total_tokens > self.PRUNE_PROTECT_TOKENS:
                    pruned_tokens += tokens
                    to_prune.append(msg)

        if pruned_tokens < self.PRUNE_MINIMUM_TOKENS:
            return 0

        pruned_count = 0
        by_call = {tc["id"]: tc["function"] for m in self._messages for tc in m.tool_calls}
        paths: set[str] = set()

        for msg in to_prune:
            fn = by_call.get(msg.tool_call_id) or {}
            if fn.get("name") == "read_file":
                try:
                    paths.add(json.loads(fn["arguments"])["path"])
                except (ValueError, KeyError, TypeError):
                    pass
            head = (msg.content or "").strip().splitlines()[:1]
            hint = f": {head[0][:100]}" if head else ""
            # name the call that made it, so a model that needs it again re-runs exactly that (and not a search for it)
            args = fn.get("arguments") or ""
            hint = f" from {fn.get('name', 'tool')}({args[:120]}){hint}" if fn else hint
            if self.archive is not None and not self.config.pi_mode:  # pi mode has no recall_result: re-running is the way back
                rid = self.archive.put(msg.content or "")
                msg.content = (
                    f"[Old tool result cleared to save tokens ({rid}, {len(msg.content or '')} chars){hint}. "
                    f"recall_result {rid} returns the exact text.]"
                )
            else:
                msg.content = f"[Old tool result cleared to save tokens{hint}. Re-run the tool if you need it again.]"
            msg.token_count = count_tokens(msg.content, self._model_name)
            msg.pruned_at = datetime.now()
            pruned_count += 1

        self.last_prune = {"results": pruned_count, "tokens_cleared": pruned_tokens, "prompt_tokens": self._latest_usage.prompt_tokens, "read_paths": sorted(paths)}
        return pruned_count

    def clear(self) -> None:
        self._messages = []
        self.summary = None
        self.files_read.clear()
        self.files_modified.clear()
