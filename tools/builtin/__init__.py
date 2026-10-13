from tools.builtin.edit_file import EditTool
from tools.builtin.git import GitTool
from tools.builtin.glob import GlobTool
from tools.builtin.grep import GrepTool
from tools.builtin.list_dir import ListDirTool
from tools.builtin.memory import MemoryTool
from tools.builtin.outline import OutlineTool
from tools.builtin.read_file import ReadFileTool
from tools.builtin.search import SearchTool
from tools.builtin.shell import ShellTool
from tools.builtin.todo import TodosTool
from tools.builtin.use_skill import UseSkillTool
from tools.builtin.web_fetch import WebFetchTool
from tools.builtin.web_search import WebSearchTool
from tools.builtin.write_file import WriteFileTool

__all__ = [
    "ReadFileTool",
    "WriteFileTool",
    "EditTool",
    "ShellTool",
    "ListDirTool",
    "GrepTool",
    "OutlineTool",
    "SearchTool",
    "GlobTool",
    "GitTool",
    "WebSearchTool",
    "WebFetchTool",
    "TodosTool",
    "MemoryTool",
    "UseSkillTool",
]


def get_all_builtin_tools() -> list[type]:
    return [
        ReadFileTool,
        WriteFileTool,
        EditTool,
        ShellTool,
        ListDirTool,
        GrepTool,
        OutlineTool,
        SearchTool,
        GlobTool,
        GitTool,
        WebSearchTool,
        WebFetchTool,
        TodosTool,
        MemoryTool,
        UseSkillTool,
    ]
