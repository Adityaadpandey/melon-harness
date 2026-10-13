from typing import Any
from client.llm_client import LLMClient
from client.response import StreamEventType, TokenUsage
from context.manager import ContextManager
from prompts.system import get_compression_prompt


class ChatCompactor:
    def __init__(self, client: LLMClient):
        self.client = client

    def _format_history_for_compaction(self, messages: list[dict[str, Any]]) -> str:
        output = ["Here is the conversation that needs to be continue: \n"]

        for msg in messages:
            role = msg.get("role", "")
            content = msg.get("content", "")

            if role == "system":
                continue

            if role == "tool":
                tool_id = msg.get("tool_call_id", "unknown")

                truncated = content[:2000] if len(content) > 2000 else content
                if len(content) > 2000:
                    truncated += "\n... [tool output truncated]"

                output.append(f"[Tool Result ({tool_id})]:\n{truncated}")
            elif role == "assistant":
                tool_details = []
                if content:
                    truncated = content[:3000] if len(content) > 3000 else content
                    if len(content) > 3000:
                        truncated += "\n... [response truncated]"
                    output.append(f"Assistant:\n{truncated}")

                if msg.get("tool_calls"):
                    for tc in msg["tool_calls"]:
                        func = tc.get("function", {})
                        name = func.get("name", "unknown")
                        args = func.get("arguments", "{}")

                        if len(args) > 500:
                            args = args[:500]
                        tool_details.append(f"  - {name}({args})")

                    output.append("Assistant called tools:\n" + "\n".join(tool_details))
            else:
                truncated = content[:1500] if len(content) > 1500 else content
                if len(content) > 1500:
                    truncated += "\n... [message truncated]"
                output.append(f"User:\n{truncated}")

        return "\n\n---\n\n".join(output)

    async def compress(
        self, context_manager: ContextManager
    ) -> tuple[str | None, TokenUsage | None, int]:
        """Summarise the older part of the history (together with any earlier
        summary), leaving the recent tail untouched. Returns (summary, usage, cut)."""
        cm = context_manager
        keep = cm.config.keep_recent_tokens
        cut = cm.find_cut(keep)
        if cut <= 0:  # the whole history is "recent": fall back to keeping a quarter
            cut = cm.find_cut(max(1, keep // 4))
        if cut <= 0:
            return None, None, 0

        history = self._format_history_for_compaction(cm.history_for_summary(cut))
        if cm.summary:
            history = f"PREVIOUS SUMMARY (update it, do not drop facts that still matter):\n{cm.summary}\n\n---\n\nNEW MESSAGES TO FOLD IN:\n{history}"
        compression_messages = [
            {"role": "system", "content": get_compression_prompt()},
            {"role": "user", "content": history},
        ]

        try:
            summary, usage = "", None
            async for event in self.client.chat_completion(compression_messages, stream=False, role="summarize"):
                if event.type == StreamEventType.MESSAGE_COMPLETE:
                    usage = event.usage
                    summary += event.text_delta.content if event.text_delta else ""
            if not summary:
                return None, None, 0
            return summary, usage, cut
        except Exception:
            return None, None, 0
