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


def test_pi_read_cuts_at_2000_lines_and_says_where_to_continue():
    from tools.builtin.read_file import pi_read

    body = "\n".join(f"l{i}" for i in range(1, 5001))
    out = pi_read(body, 1, None)
    assert out.startswith("l1\n") and "l2000\n" in out and "l2001" not in out
    assert out.endswith("[Showing lines 1-2000 of 5000. Use offset=2001 to continue.]")
    assert pi_read(body, 4990, None) == "\n".join(f"l{i}" for i in range(4990, 5001))
    assert pi_read("a\nb\nc", 1, 2).endswith("[1 more lines in file. Use offset=3 to continue.]")
    assert "beyond end" in pi_read("a", 9, None)


def test_pi_prompt_is_small_and_lists_only_the_tools_given(tmp_path):
    from prompts.system import get_system_prompt
    from tools.registry import create_default_registry
    from utils.text import count_tokens

    cfg = Config(cwd=tmp_path)
    tools = create_default_registry(cfg).get_tools()
    prompt = get_system_prompt(cfg, "remembered", tools, [])
    assert len(tools) == 7 and "- shell:" in prompt and "- git:" not in prompt and "remembered" not in prompt
    assert count_tokens(prompt, cfg.model_name) < 400
