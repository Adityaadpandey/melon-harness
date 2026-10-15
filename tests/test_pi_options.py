import subprocess

from config.config import Config
from context.manager import ContextManager
from prompts.system import get_system_prompt
from tools.registry import create_default_registry


def test_core_tools_only_gives_pis_four(tmp_path):
    assert Config(cwd=tmp_path, core_tools_only=True).allowed_tools == ["read_file", "write_file", "edit", "shell"]


def test_repo_map_is_opt_in(tmp_path):
    (tmp_path / "core.py").write_text("def start():\n    pass\n")
    subprocess.run(["git", "init", "-q"], cwd=tmp_path)
    subprocess.run(["git", "add", "."], cwd=tmp_path)
    assert "<repo_map>" not in get_system_prompt(Config(cwd=tmp_path), None, [], [])
    assert "core.py: start" in get_system_prompt(Config(cwd=tmp_path, repo_map=True), None, [], [])


def test_compaction_leaves_the_cacheable_prefix_alone(tmp_path):
    """System prompt and tool order must be byte-identical before and after compaction, so the provider's cache still hits."""
    cfg = Config(cwd=tmp_path)
    cm = ContextManager(cfg, None, create_default_registry(cfg).get_tools())
    for i in range(6):
        cm.add_user_message(f"q{i}")
        cm.add_assistant_message(f"a{i}")
    before = cm.get_messages()[0]
    cm.apply_compaction("summary", 8)
    assert cm.get_messages()[0] == before
    names = [t.name for t in create_default_registry(cfg).get_tools()]
    assert names == [t.name for t in create_default_registry(cfg).get_tools()]
