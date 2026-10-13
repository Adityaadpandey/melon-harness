from datetime import datetime
import json
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
    PRUNE_PROTECT_TOKENS = 40_000
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

    @property
    def message_count(self) -> int:
        return len(self._messages)

    def add_user_message(self, content: str) -> None:
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

    def needs_compression(self) -> bool:
        # a 1M window doesn't mean 800k tokens of history is a good idea:
        # quality drops and every turn resends it, so cap with compact_at_tokens
        limit = min(self.context_window * 0.8, self.config.compact_at_tokens)
        return self._latest_usage.total_tokens > limit

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
        return [m.to_dict() for m in self._messages[:cut]]

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

    def apply_compaction(self, summary: str, cut: int) -> None:
        """Replace messages[:cut] with the summary and keep the recent tail verbatim,
        so the model still sees exactly what it just did instead of a retelling."""
        self._track_files(self._messages[:cut])
        files = ""
        if self.files_modified:
            files += f"\n\nFiles modified so far: {', '.join(sorted(self.files_modified))}"
        reads = sorted(self.files_read - self.files_modified)
        if reads:
            files += f"\nFiles read so far: {', '.join(reads[:40])}"
        self.summary = summary + files
        content = (
            "# Earlier conversation (compacted)\n\n"
            "Older messages were replaced by this summary to save context. Actions listed as "
            "completed are done; do not repeat them. The most recent messages follow unchanged.\n\n"
            f"{self.summary}"
        )
        head = MessageItem(role="user", content=content, token_count=count_tokens(content, self._model_name))
        self._messages = [head] + self._messages[cut:]

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
        user_message_count = sum(1 for msg in self._messages if msg.role == "user")

        if user_message_count < 2:
            return 0

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

        for msg in to_prune:
            msg.content = "[Old tool result content cleared]"
            msg.token_count = count_tokens(msg.content, self._model_name)
            msg.pruned_at = datetime.now()
            pruned_count += 1

        return pruned_count

    def clear(self) -> None:
        self._messages = []
        self.summary = None
        self.files_read.clear()
        self.files_modified.clear()
