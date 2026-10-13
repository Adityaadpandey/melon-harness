
from config.config import Config
from ui.tui import TUI, get_console


def test_args_table_renders_non_string_values(tmp_path):
    tui = TUI(Config(cwd=tmp_path), get_console())
    args = {"path": "a.py", "limit": 50, "offset": 0, "flag": True, "none": None, "items": [1, "x"], "d": {"k": 1}}
    tui.tool_call_start("call_12345678", "read_file", "read", args)  # used to raise NotRenderableError
