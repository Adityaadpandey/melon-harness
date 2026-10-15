from config.config import Config


def test_pi_mode_gives_the_seven_plain_tools_and_no_call_cap(tmp_path):
    c = Config(cwd=tmp_path)
    assert c.allowed_tools == ["read_file", "write_file", "edit", "shell", "grep", "glob", "list_dir"]
    assert c.read_call_budget == 0 and not c.check_references and not c.auto_scout


def test_anything_set_by_hand_wins_over_pi_defaults(tmp_path):
    c = Config(cwd=tmp_path, read_call_budget=5, allowed_tools=["git"])
    assert c.read_call_budget == 5 and c.allowed_tools == ["git"]
    assert Config(cwd=tmp_path, pi_mode=False).allowed_tools is None


def test_pi_mode_compacts_only_near_the_window_and_never_clears_results(tmp_path):
    from context.manager import ContextManager

    cm = ContextManager(Config(cwd=tmp_path), None, None)
    cm.context_window = 100_000
    assert cm._limit() == 100_000 - 16_384
    cm.add_assistant_message("", [{"id": "a", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}])
    cm.add_tool_result("a", "x" * 400_000)
    assert cm.prune_tool_outputs() == 0
