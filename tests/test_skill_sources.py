import json
from pathlib import Path

import pytest

from agent.skills import load_skills, match_skills
from config.config import Config
from prompts.system import get_system_prompt
from safety.guard import violation
from tools.registry import create_default_registry

pytestmark = pytest.mark.real_skill_dirs


@pytest.fixture
def home(tmp_path, monkeypatch):
    """A fake home with one Claude Code plugin, ~/.claude/skills (including a dead link) and a repo."""
    h = tmp_path / "home"
    plugin = h / ".claude" / "plugins" / "cache" / "acme" / "1.0"
    for name, head in [
        ("tidy", "name: tidy\ndescription: >\n  Keeps diffs small\n  and readable"),
        ("ponytail", "name: ponytail\ndescription: the plugin's long Claude Code version"),
    ]:
        (plugin / "skills" / name).mkdir(parents=True)
        (plugin / "skills" / name / "SKILL.md").write_text(f"---\n{head}\n---\nbody of {name}\n")
    (h / ".claude" / "plugins" / "installed_plugins.json").write_text(json.dumps({"plugins": {"acme@x": [{"installPath": str(plugin)}]}}))
    (h / ".claude" / "skills" / "review").mkdir(parents=True)
    (h / ".claude" / "skills" / "review" / "SKILL.md").write_text('---\nname: review\ndescription: "Review a diff"\n---\nreview body\n')
    (h / ".claude" / "skills" / "gone").mkdir()
    (h / ".claude" / "skills" / "gone" / "SKILL.md").symlink_to(tmp_path / "missing.md")
    (h / "repo").mkdir()
    (h / ".claude" / "settings.json").write_text("{}")
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: h))
    monkeypatch.setenv("HOME", str(h))  # ~ in a path expands from $HOME
    return h


def test_claude_code_skills_load_without_hijacking_requests_or_mews_own_ponytail(home):
    sk = {s.name: s for s in load_skills(Config(cwd=home / "repo"))}  # the dead link is skipped, not a crash
    assert sk["tidy"].description == "Keeps diffs small and readable" and sk["review"].description == "Review a diff"
    assert sk["ponytail"].always and "Claude Code" not in sk["ponytail"].description  # mew's compact one wins
    assert "review" not in [s.name for s in match_skills(list(sk.values()), "review this diff")]  # listed, never auto-fired


def test_pi_prompt_inlines_always_on_skills_and_lists_the_rest_once_per_folder(home):
    c = Config(cwd=home / "repo")
    prompt = get_system_prompt(c, None, create_default_registry(c).get_tools(), load_skills(c))
    assert "the least that fully works" in prompt  # ponytail applies on every request, no read needed
    assert "~/.claude/skills/\n- review/SKILL.md: Review a diff" in prompt  # machine-wide skills say when to use them too
    assert prompt.count("/acme/1.0/skills/") == 1 and "- tidy/SKILL.md" in prompt


def test_skill_files_are_readable_from_any_repo_but_nothing_else_outside_it(home):
    c = Config(cwd=home / "repo")
    assert violation("read_file", {"path": "~/.claude/skills/review/SKILL.md"}, c) is None
    assert violation("read_file", {"path": str(home / ".claude" / "settings.json")}, c)
    assert violation("write_file", {"path": "~/.claude/skills/review/SKILL.md", "content": "x"}, c)
