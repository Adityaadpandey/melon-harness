import asyncio
from pathlib import Path

from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from utils.paths import resolve_path
from utils.rank import rank_files
from utils.search import list_files


class SearchParams(BaseModel):
    query: str = Field(..., description="What you are looking for, in plain words or identifiers, e.g. 'event description blank line section markdown'")
    path: str = Field(".", description="Directory to search (default: working directory)")
    limit: int = Field(8, ge=1, le=20, description="How many files to return")


class SearchTool(Tool):
    name = "search"
    description = (
        "Find which files are about something. Ranks files by relevance to a plain-words query (BM25 over file paths "
        "and contents, camelCase aware) and shows the best matching lines of each. Use it FIRST when you do not know "
        "where something lives; use grep afterwards for exact strings."
    )
    kind = ToolKind.READ
    schema = SearchParams

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = SearchParams(**invocation.params)
        root = resolve_path(invocation.cwd, p.path)
        if not root.is_dir():
            return ToolResult.error_result(f"Directory does not exist: {root}")
        files = await asyncio.to_thread(list_files, root, self.config.forbidden_paths)
        try:
            from agent.learn import Learned

            learned = Learned(self.config.cwd)
            prior = {k: v for k, v in learned.prior(p.query).items()}
        except Exception:
            prior = {}
        ranked = await asyncio.to_thread(rank_files, root, files, p.query, p.limit, 3, prior)
        if not ranked:
            return ToolResult.success_result(f"No files match '{p.query}' in {len(files)} files. Try other words or grep.")
        out = []
        for rel, score, lines in ranked:
            out.append(f"{rel}  ({score})")
            out.extend(f"  {n}: {text}" for n, text in lines)
        return ToolResult.success_result("\n".join(out), metadata={"files": len(ranked), "searched": len(files)})
