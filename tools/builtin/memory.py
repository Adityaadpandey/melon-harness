import hashlib
import json
import os
from pathlib import Path
from config.config import Config
from config.loader import get_data_dir
from utils.paths import atomic_write
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from pydantic import BaseModel, Field


class MemoryParams(BaseModel):
    action: str = Field(
        ..., description="Action: 'set', 'get', 'delete', 'list', 'clear'"
    )
    key: str | None = Field(
        None, description="Memory key (required for `set`, `get`, `delete`)"
    )
    value: str | None = Field(None, description="Value to store (required for `set`)")


class MemoryTool(Tool):
    name = "memory"
    description = "Store and retrieve persistent memory for this workspace only. Use this to remember user preferences, important context or notes."
    kind = ToolKind.MEMORY
    schema = MemoryParams

    def _path(self) -> Path:
        """One file per workspace, so notes about one project never show up in another.
        e.g. /work/shop and /work/blog get different files under <data dir>/memory/."""
        root = os.path.realpath(self.config.cwd)
        return get_data_dir() / "memory" / f"{hashlib.sha256(root.encode()).hexdigest()[:16]}.json"

    def _load_memory(self) -> dict:
        try:
            return json.loads(self._path().read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return {"entries": {}}

    def _save_memory(self, memory: dict) -> None:
        path = self._path()
        path.parent.mkdir(parents=True, exist_ok=True)
        atomic_write(path, json.dumps(memory, indent=2, ensure_ascii=False))

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        params = MemoryParams(**invocation.params)

        if params.action.lower() == "set":
            if not params.key or not params.value:
                return ToolResult.error_result(
                    "`key` and `value` are required for 'set' action"
                )
            memory = self._load_memory()
            memory["entries"][params.key] = params.value
            self._save_memory(memory)

            return ToolResult.success_result(f"Set memory: {params.key}")
        elif params.action.lower() == "get":
            if not params.key:
                return ToolResult.error_result("`key` required for 'get' action")

            memory = self._load_memory()
            if params.key not in memory.get("entries", {}):
                return ToolResult.success_result(
                    f"Memory not found: {params.key}",
                    metadata={
                        "found": False,
                    },
                )
            return ToolResult.success_result(
                f"Memory found: {params.key}: {memory['entries'][params.key]}",
                metadata={
                    "found": True,
                },
            )
        elif params.action == "delete":
            if not params.key:
                return ToolResult.error_result("`key` required for 'get' action")
            memory = self._load_memory()
            if params.key not in memory.get("entries", {}):
                return ToolResult.success_result(f"Memory not found: {params.key}")

            del memory["entries"][params.key]
            self._save_memory(memory)

            return ToolResult.success_result(f"Deleted memory: {params.key}")
        elif params.action == "list":
            memory = self._load_memory()
            entries = memory.get("entries", {})
            if not entries:
                return ToolResult.success_result(
                    f"No memories stored",
                    metadata={
                        "found": False,
                    },
                )
            lines = [f"Stored memories:"]
            for key, value in sorted(entries.items()):
                lines.append(f"  {key}: {value}")

            return ToolResult.success_result(
                "\n".join(lines),
                metadata={
                    "found": True,
                },
            )
        elif params.action == "clear":
            memory = self._load_memory()
            count = len(memory.get("entries", {}))
            memory["entries"] = {}
            self._save_memory(memory)
            return ToolResult.success_result(f"Cleared {count} memory entries")
        else:
            return ToolResult.error_result(f"Unknown action: {params.action}")
