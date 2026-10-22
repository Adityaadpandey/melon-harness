"""Near-miss tool calls from open models are fixed, not bounced back."""
from client.response import parse_tool_call_arguments as parse
from tools.builtin.edit_file import EditParams
from tools.builtin.read_file import ReadFileParams
from tools.repair import fix_arguments, fix_tool_name

OFFERED = {"read_file", "write_file", "edit", "shell", "grep", "glob", "list_dir", "todos"}


def test_almost_json_is_repaired():
    assert parse('{"pattern": "\\d+"}') == {"pattern": "\\d+"}  # lone backslash from a regex
    assert parse('{"content": "a\nb"}') == {"content": "a\nb"}  # raw newline inside a string
    assert parse('```json\n{"path": "a"}\n```') == {"path": "a"}
    assert parse('"{\\"path\\": \\"a\\"}"') == {"path": "a"}  # encoded twice
    assert parse('{"path": "a') == {"raw_arguments": '{"path": "a'}  # truncated: left for the model to re-send


def test_tool_names_from_other_harnesses_map_to_ours():
    assert fix_tool_name("Read", OFFERED) == "read_file"
    assert fix_tool_name("Bash", OFFERED) == "shell"
    assert fix_tool_name("str_replace", OFFERED) == "edit"
    assert fix_tool_name("GREP", OFFERED) == "grep"
    assert fix_tool_name("web_search", OFFERED) == "web_search"  # not offered: unchanged, the error lists the real tools


def test_argument_slips_are_fixed_only_where_the_schema_allows_one_reading():
    assert fix_arguments(EditParams, {"file_path": "a.py", "old_str": "x", "new_str": "y"}) == {
        "path": "a.py", "old_string": "x", "new_string": "y"}
    assert fix_arguments(EditParams, {"filePath": "a.py", "oldString": "x", "newString": "y", "replaceAll": True})["replace_all"] is True
    # GLM sends the edits list as a JSON string; others send one edit without the list
    as_text = fix_arguments(EditParams, {"path": "a", "edits": '[{"old_string": "x", "new_string": "y"}]'})
    assert as_text["edits"] == [{"old_string": "x", "new_string": "y"}]
    single = fix_arguments(EditParams, {"path": "a", "edits": {"old_string": "x", "new_string": "y"}})
    assert single["edits"] == [{"old_string": "x", "new_string": "y"}]
    EditParams(**single)
    assert fix_arguments(ReadFileParams, {"path": "a", "limit": None}) == {"path": "a"}
    # a string field that happens to hold JSON is left alone
    assert fix_arguments(EditParams, {"path": "a", "old_string": "{}", "new_string": "[1]"})["new_string"] == "[1]"
