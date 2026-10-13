from pathlib import Path

from pydantic import BaseModel, Field

from tools.base import FileDiff, Tool, ToolConfirmation, ToolInvocation, ToolKind, ToolResult
from utils.edits import plan_edits
from utils.paths import ensure_parent_directory, resolve_path


class EditItem(BaseModel):
    old_string: str = Field(..., description="Exact text to replace; unique in the file")
    new_string: str = Field(..., description="Replacement text (may be empty to delete)")


class EditParams(BaseModel):
    path: str = Field(..., description="File to edit (relative to the working directory, or absolute)")
    old_string: str = Field("", description="Exact text to replace. Empty only when creating a new file.")
    new_string: str = Field("", description="Replacement text. Empty deletes the matched text.")
    replace_all: bool = Field(False, description="Replace every occurrence of old_string")
    edits: list[EditItem] | None = Field(
        None,
        description="Several replacements in ONE call (preferred when changing multiple places in a file). "
        "Each is matched against the original file, must be unique, and must not overlap another. "
        "When given, old_string/new_string are ignored.",
    )


def _read(path: Path) -> str:
    # newline="" keeps CRLF intact; read_text() would silently normalise it
    with open(path, encoding="utf-8", errors="replace", newline="") as f:
        return f.read()


class EditTool(Tool):
    name = "edit"
    description = (
        "Edit a file by exact text replacement. Use `edits` for several changes in one call. "
        "Trailing-whitespace and smart-quote differences are tolerated; anything else must match exactly. "
        "To create or fully rewrite a file use write_file."
    )
    kind = ToolKind.WRITE
    schema = EditParams

    def _pairs(self, p: EditParams) -> list[tuple[str, str]]:
        return [(e.old_string, e.new_string) for e in p.edits] if p.edits else [(p.old_string, p.new_string)]

    def _plan(self, invocation: ToolInvocation):
        """(path, old_content, new_content, error, count) shared by preview and execution."""
        p = EditParams(**invocation.params)
        path = resolve_path(invocation.cwd, p.path)
        if not path.exists():
            if p.old_string and not p.edits:
                return path, "", None, f"File does not exist: {path}. To create a file, leave old_string empty (or use write_file).", 0
            return path, "", p.new_string, None, 1  # new file
        if not path.is_file():
            return path, "", None, f"Not a file: {path}", 0
        old = _read(path)
        new, err, n = plan_edits(old, self._pairs(p), p.replace_all)
        return path, old, new, err, n

    async def get_confirmation(self, invocation: ToolInvocation) -> ToolConfirmation | None:
        path, old, new, err, _ = self._plan(invocation)
        if err:
            return None  # nothing will be written, so there is nothing to approve; execute() reports the error
        is_new = not path.exists()
        return ToolConfirmation(
            tool_name=self.name,
            params=invocation.params,
            description=f"{'Create' if is_new else 'Edit'} {path}",
            diff=FileDiff(path=path, old_content=old, new_content=new, is_new_file=is_new),
            affected_paths=[path],
        )

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        path, old, new, err, n = self._plan(invocation)
        if err:
            return ToolResult.error_result(err)
        is_new = not path.exists()
        try:
            ensure_parent_directory(path)
            with open(path, "w", encoding="utf-8", newline="") as f:
                f.write(new)
        except OSError as e:
            return ToolResult.error_result(f"Failed to write file: {e}")
        delta = len(new.splitlines()) - len(old.splitlines())
        note = f" ({delta:+d} lines)" if delta else ""
        verb = "Created" if is_new else f"Edited ({n} replacement{'s' if n != 1 else ''})"
        return ToolResult.success_result(
            f"{verb} {path}{note}",
            diff=FileDiff(path=path, old_content=old, new_content=new, is_new_file=is_new),
            metadata={"path": str(path), "replaced_count": n, "line_diff": delta, "is_new_file": is_new},
        )
