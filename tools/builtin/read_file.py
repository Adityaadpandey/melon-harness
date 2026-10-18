import difflib
import re
from pathlib import Path

from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from utils.paths import is_binary_file, resolve_path
from utils.search import similar_paths
from utils.text import count_tokens, truncate_text


WHOLE_FILE_LINES = 400
MIN_SLICE_LINES = 200


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


PI_MAX_LINES = 2000
PI_MAX_BYTES = 50 * 1024


def pi_read(content: str, offset: int, limit: int | None) -> str:
    """Pi's read: plain text from `offset`, cut at 2000 lines or 50KB (whichever comes first, never mid-line),
    with the exact offset to continue from. Example: a 5000-line file gives lines 1-2000, then
    '[Showing lines 1-2000 of 5000. Use offset=2001 to continue.]'."""
    all_lines = content.split("\n")
    total = len(all_lines)
    start = max(0, offset - 1)
    if start >= total:
        return f"Offset {offset} is beyond end of file ({total} lines total)"
    end = min(start + limit, total) if limit else total
    picked, size = [], 0
    for line in all_lines[start:end][:PI_MAX_LINES]:
        size += len(line.encode("utf-8")) + (1 if picked else 0)
        if size > PI_MAX_BYTES:
            break
        picked.append(line)
    if not picked:
        return f"[Line {start + 1} is over {PI_MAX_BYTES // 1024}KB. Use shell: sed -n '{start + 1}p' <file> | head -c {PI_MAX_BYTES}]"
    shown_to = start + len(picked)
    text = "\n".join(picked)
    if shown_to < end:  # our cap stopped it
        return f"{text}\n\n[Showing lines {start + 1}-{shown_to} of {total}. Use offset={shown_to + 1} to continue.]"
    if shown_to < total:  # the caller's own limit stopped it, but the file goes on
        return f"{text}\n\n[{total - shown_to} more lines in file. Use offset={shown_to + 1} to continue.]"
    return text


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
        "Files up to 400 lines come whole, so just call it with the path. For longer files pass `symbol` (see `outline`) or a wide offset/limit; many small slices cost more than one read. "
        "Cannot read binary files (images, executables, etc.). Read a file before you edit it. Do not re-read a file you just edited: the edit would have failed if it had not applied."
    )
    kind = ToolKind.READ

    schema = ReadFileParams

    def __init__(self, config):
        super().__init__(config)
        if config.pi_mode:
            self.description = (
                f"Read the contents of a text file. Output is truncated to {PI_MAX_LINES} lines or {PI_MAX_BYTES // 1024}KB "
                "(whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete."
            )
        self._seen: dict[str, tuple[tuple, list[tuple[int, int]]]] = {}
        # whole-file text the model last got, kept here (not in the prompt) so a changed
        # file can be answered with a diff instead of being sent again in full
        self._snap: dict[str, tuple[int, list[str]]] = {}

    def forget(self, path: Path) -> None:
        """The model no longer has this file's text (its read result was cleared): the next read sends it again."""
        self._seen.pop(str(path), None)
        self._snap.pop(str(path), None)

    def _coverage(self, path: Path, epoch: int) -> list[tuple[int, int]]:
        st = path.stat()
        stamp = (st.st_mtime_ns, st.st_size, epoch)
        entry = self._seen.get(str(path))
        if not entry or entry[0] != stamp:
            entry = self._seen[str(path)] = (stamp, [])
        return entry[1]

    def _pi_incremental(self, path: Path, content: str, params, epoch: int) -> str:
        """Whole-file reads only: a file the model already holds is not sent again. Unchanged gives one line,
        changed gives a diff against what it last saw. Slices and files over pi's cap go out as plain pi reads.
        Example: reading a 300-line file twice costs the 300 lines once and ~25 tokens the second time."""
        text = pi_read(content, params.offset, params.limit)
        if params.offset != 1 or params.limit is not None:
            return text
        key, lines = str(path), content.split("\n")
        snap = self._snap.get(key)
        if snap and snap[0] == epoch:
            if snap[1] == lines:
                return f"[{path.name} is unchanged since you last read it ({len(lines)} lines) and is still in context above]"
            diff = "\n".join(difflib.unified_diff(snap[1], lines, "before", "now", n=2, lineterm=""))
            if len(diff) < 0.6 * len(content):
                self._snap[key] = (epoch, lines)
                return f"[{path.name} changed since you last read it ({len(lines)} lines now); only the changes are shown, the rest is as you saw it]\n{diff}"
        if "Use offset=" not in text:  # a cut-off read leaves the model without the whole file, so nothing to dedup against
            self._snap[key] = (epoch, lines)
        return text

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

            if self.config.pi_mode:
                return ToolResult.success_result(self._pi_incremental(path, content, params, invocation.epoch))

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

            # Small slices cost an extra model round trip each (the whole history is sent again), far more than
            # a few more lines: a 400-line file is read whole, and any other slice grows to a useful size.
            # Lines the model already has are skipped below, so widening never re-sends them.
            if not params.symbol:
                if total_lines <= WHOLE_FILE_LINES:
                    start_idx, end_idx = 0, total_lines
                else:
                    end_idx = min(total_lines, max(end_idx, start_idx + MIN_SLICE_LINES))

            # Per-file map of the line ranges already in context. A read only returns
            # lines the model does not have yet; changing the file (mtime/size) or
            # losing the old output (epoch: pruned or compacted) forgets the map.
            snap = self._snap.get(str(path))
            old_lines = snap[1] if snap and snap[0] == invocation.epoch else None
            have = self._coverage(path, invocation.epoch)
            if old_lines is not None and not have and params.offset == 1 and params.limit is None and lines != old_lines:
                diff = "\n".join(difflib.unified_diff(old_lines, lines, "before", "now", n=2, lineterm=""))
                if len(diff) < 0.6 * len(content):
                    have.append((1, total_lines))
                    self._snap[str(path)] = (invocation.epoch, lines)
                    return ToolResult.success_result(
                        f"[{path.name} changed since you last read it ({total_lines} lines now); "
                        f"only the changes are shown, the rest is as you saw it]\n{diff}",
                        metadata={"path": str(path), "total_lines": total_lines, "diff_only": True},
                    )
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

            if have == [(1, total_lines)]:
                self._snap[str(path)] = (invocation.epoch, lines)

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
