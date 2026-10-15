from config.config import Config


def test_pi_mode_gives_the_seven_plain_tools_and_no_call_cap(tmp_path):
    c = Config(cwd=tmp_path)
    assert c.allowed_tools == ["read_file", "write_file", "edit", "shell", "grep", "glob", "list_dir"]
    assert c.read_call_budget == 0 and not c.check_references and not c.auto_scout


def test_anything_set_by_hand_wins_over_pi_defaults(tmp_path):
    c = Config(cwd=tmp_path, read_call_budget=5, allowed_tools=["git"])
    assert c.read_call_budget == 5 and c.allowed_tools == ["git"]
    assert Config(cwd=tmp_path, pi_mode=False).allowed_tools is None
