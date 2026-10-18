from tools.base import (
    FileDiff,
    Tool,
    ToolConfirmation,
    ToolInvocation,
    ToolKind,
    ToolResult,
)
from pydantic import BaseModel, Field

from utils.paths import atomic_write, ensure_parent_directory, resolve_path


class WriteFileParams(BaseModel):
    path: str = Field(
        ...,
        description="Path to the file to write (relative to working directory or absolute)",
    )
    content: str = Field(..., description="Content to write to the file")
    create_directories: bool = Field(
        True, description="Create parent directories if they don't exist"
    )


class WriteFileTool(Tool):
    name = "write_file"
    description = (
        "Write content to a file. Creates the file if it doesn't exist, "
        "or overwrites if it does. Parent directories are created automatically. "
        "Use this for creating new files or completely replacing file contents. "
        "For partial modifications, use the edit tool instead. An existing file must be read first. Do not create docs or README files unless asked."
    )
    kind = ToolKind.WRITE
    schema = WriteFileParams

    async def get_confirmation(
        self, invocation: ToolInvocation
    ) -> ToolConfirmation | None:
        params = WriteFileParams(**invocation.params)
        path = resolve_path(invocation.cwd, params.path)

        is_new_file = not path.exists()

        old_content = ""
        if not is_new_file:
            try:
                old_content = path.read_text(encoding="utf-8")
            except:
                pass

        diff = FileDiff(
            path=path,
            old_content=old_content,
            new_content=params.content,
            is_new_file=is_new_file,
        )

        action = "Created" if is_new_file else "Updated"

        return ToolConfirmation(
            tool_name=self.name,
            params=invocation.params,
            description=f"{action} file: {path}",
            diff=diff,
            affected_paths=[path],
            is_dangerous=not is_new_file,
        )

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        params = WriteFileParams(**invocation.params)
        path = resolve_path(invocation.cwd, params.path)

        is_new_file = not path.exists()
        old_content = ""

        if not is_new_file:
            try:
                old_content = path.read_text(encoding="utf-8")
            except:
                pass

        # formatters (biome, prettier, black) fail a file that ends without "\n"; models often forget it
        if params.content and not params.content.endswith("\n") and (is_new_file or old_content.endswith("\n")):
            params.content += "\n"

        try:
            if params.create_directories:
                ensure_parent_directory(path)
            elif not path.parent.exists():
                return ToolResult.error_result(
                    f"Parent directory does not exist: {path.parent}"
                )

            atomic_write(path, params.content)

            action = "Created" if is_new_file else "Updated"
            line_count = len(params.content.splitlines())

            return ToolResult.success_result(
                f"{action} {path} {line_count} lines",
                diff=FileDiff(
                    path=path,
                    old_content=old_content,
                    new_content=params.content,
                    is_new_file=is_new_file,
                ),
                metadata={
                    "path": str(path),
                    "is_new_file": is_new_file,
                    "lines": line_count,
                    "bytes": len(params.content.encode("utf-8")),
                },
            )
        except OSError as e:
            return ToolResult.error_result(f"Failed to write file: {e}")
