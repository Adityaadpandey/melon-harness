import re
from pathlib import Path

from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from utils.paths import is_binary_file, resolve_path
from utils.search import similar_paths
from utils.text import count_tokens, truncate_text


class ReadFileParams(BaseModel):

    path: str = Field(
        ...,
        description="Path to the file to read (relative to working directory or absolute)",
    )

    offset: int = Field(
        1,
        ge=1,
        description="Line number to start reading from (1-based). Defaults to 1",
    )

    limit: int | None = Field(
        None,
        ge=1,
        description="Maximum number of lines to read. If not specified, reads entire file.",
    )

    symbol: str | None = Field(
        None,
        description="Read just this function/class/method (see `outline`) instead of a line range",
    )


def _missing(have: list[tuple[int, int]], start: int, end: int) -> list[tuple[int, int]]:
    """Sub-ranges of [start, end] (1-based, inclusive) not covered by `have`."""
    out, cur = [], start
    for a, b in sorted(have):
        if b < cur:
            continue
        if a > end:
            break
        if a > cur:
            out.append((cur, a - 1))
        cur = max(cur, b + 1)
    if cur <= end:
        out.append((cur, end))
    return out


def _add(have: list[tuple[int, int]], a: int, b: int) -> None:
    have.append((a, b))
    have.sort()
    merged = [have[0]]
    for x, y in have[1:]:
        if x <= merged[-1][1] + 1:
            merged[-1] = (merged[-1][0], max(merged[-1][1], y))
        else:
            merged.append((x, y))
    have[:] = merged


class ReadFileTool(Tool):
    name = "read_file"
    description = (
        "Read the contents of a text file. Returns the file content with line numbers. "
        "For large files, use offset and limit to read specific portions. "
        "Cannot read binary files (images, executables, etc.)."
    )
    kind = ToolKind.READ

    schema = ReadFileParams

    def __init__(self, config):
        super().__init__(config)
        self._seen: dict[str, tuple[tuple, list[tuple[int, int]]]] = {}

    def _coverage(self, path: Path, epoch: int) -> list[tuple[int, int]]:
        st = path.stat()
        stamp = (st.st_mtime_ns, st.st_size, epoch)
        entry = self._seen.get(str(path))
        if not entry or entry[0] != stamp:
            entry = self._seen[str(path)] = (stamp, [])
        return entry[1]

    MAX_FILE_SIZE = 1024 * 1024 * 10
    MAX_OUTPUT_TOKENS = 8000  # ~32KB; bigger files are read in slices with offset/limit

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        params = ReadFileParams(**invocation.params)
        path = resolve_path(invocation.cwd, params.path)

        if not path.exists():
            return ToolResult.error_result(
                f"File not found: {path}." + similar_paths(Path(invocation.cwd), path, self.config.forbidden_paths)
            )

        if params.symbol:
            from utils.outline import outline

            try:
                syms = [x for x in outline(path.name, path.read_text(encoding="utf-8", errors="replace")) if x.name == params.symbol]
            except OSError:
                syms = []
            if not syms:
                return ToolResult.error_result(f"No symbol '{params.symbol}' in {path.name}. Call outline to list them.")
            if len(syms) > 1:
                where = ", ".join(f"{x.start}-{x.end}" for x in syms)
                return ToolResult.error_result(f"'{params.symbol}' appears {len(syms)} times ({where}); read one with offset/limit.")
            params.offset, params.limit = syms[0].start, min(syms[0].end - syms[0].start + 1, 400)

        if not path.is_file():
            return ToolResult.error_result(f"Path is not a file: {path}")

        file_size = path.stat().st_size

        if file_size > self.MAX_FILE_SIZE:
            return ToolResult.error_result(
                f"File too large ({file_size / (1024*1024):.1f}MB). "
                f"Maximum is {self.MAX_FILE_SIZE / (1024*1024):.0f}MB."
            )

        if is_binary_file(path):
            file_size_mb = file_size / (1024 * 1024)
            size_str = (
                f"{file_size_mb:.2f}MB" if file_size_mb >= 1 else f"{file_size} bytes"
            )
            return ToolResult.error_result(
                f"Cannot read binary file: {path.name} ({size_str}) "
                f"This tool only reads text files."
            )

        try:
            try:
                content = path.read_text(encoding="utf-8")
            except UnicodeDecodeError:
                content = path.read_text(encoding="latin-1")

            lines = content.splitlines()
            total_lines = len(lines)

            if total_lines == 0:
                return ToolResult.success_result(
                    "File is empty.",
                    metadata={
                        "lines": 0,
                    },
                )

            start_idx = max(0, params.offset - 1)

            if params.limit is not None:
                end_idx = min(start_idx + params.limit, total_lines)
            else:
                end_idx = total_lines

            # Per-file map of the line ranges already in context. A read only returns
            # lines the model does not have yet; changing the file (mtime/size) or
            # losing the old output (epoch: pruned or compacted) forgets the map.
            have = self._coverage(path, invocation.epoch)
            need = _missing(have, start_idx + 1, end_idx)
            if not need:
                return ToolResult.success_result(
                    f"[lines {start_idx + 1}-{end_idx} of {path.name} are unchanged and already in context above]",
                    metadata={"deduped": True},
                )
            formatted_lines = []
            for n, (a, b) in enumerate(need):
                if n or a > start_idx + 1:
                    formatted_lines.append(f"[lines {start_idx + 1 if not n else need[n - 1][1] + 1}-{a - 1} already in context above]")
                for i in range(a, b + 1):
                    formatted_lines.append(f"{i}|{lines[i - 1]}")
            if need[-1][1] < end_idx:
                formatted_lines.append(f"[lines {need[-1][1] + 1}-{end_idx} already in context above]")

            output = "\n".join(formatted_lines)
            token_count = count_tokens(output)

            truncated = False
            if token_count > self.MAX_OUTPUT_TOKENS:
                output = truncate_text(
                    output,
                    "gpt-4",
                    self.MAX_OUTPUT_TOKENS,
                    suffix=f"\n... [truncated; file has {total_lines} lines. Re-read a slice with offset/limit]",
                )
                truncated = True

            shown = re.findall(r"^(\d+)\|", output, re.M)  # what actually reached the model (truncation may cut it)
            if shown:
                last = int(shown[-1])
                for a, b in need:
                    _add(have, a, min(b, last))

            metadata_lines = []
            if start_idx > 0 or end_idx < total_lines:
                metadata_lines.append(
                    f"Showing lines {start_idx+1}-{end_idx} of {total_lines}"
                )

            if metadata_lines:
                header = " | ".join(metadata_lines) + "\n\n"
                output = header + output

            return ToolResult.success_result(
                output=output,
                truncated=truncated,
                metadata={
                    "path": str(path),
                    "total_lines": total_lines,
                    "shown_start": start_idx + 1,
                    "shown_end": end_idx,
                },
            )
        except Exception as e:
            return ToolResult.error_result(f"Failed to read file: {e}")
