"""The prompt fits the model it is sent to: open-model notes where they help, a skill list sized to the window."""
from pathlib import Path

from agent.skills import Skill
from config.config import Config
from prompts.system import get_system_prompt, model_notes
from tools.registry import create_default_registry


def prompt(model, window=256_000, skills=(), pi=True):
    cfg = Config(cwd="/tmp", pi_mode=pi)
    cfg.model_name, cfg.model.context_window = model, window
    return get_system_prompt(cfg, None, create_default_registry(cfg).get_tools(), list(skills))


def test_open_models_get_the_notes_tuned_ones_do_not():
    assert "written as text" in prompt("deepseek/deepseek-v4-pro") and "written as text" in prompt("openai/gpt-oss-120b")
    assert "written as text" not in prompt("claude-sonnet-5-5") and "written as text" not in prompt("codex/gpt-5.6-luna")
    assert "Make minimal changes" in prompt("moonshotai/Kimi-K3")
    assert "written as text" in prompt("Qwen/Qwen3.8-Max", pi=False)  # the guarded prompt too
    assert model_notes("claude-opus-5-5") == []


def test_the_skill_list_shrinks_to_fit_a_small_window():
    skills = [Skill(f"skill-{i}", "Use when the user asks about topic number %d and wants it done carefully." % i, "",
                    path=Path(f"/x/skills/skill-{i}/SKILL.md")) for i in range(100)]
    big, small = prompt("m", 1_000_000, skills), prompt("m", 32_000, skills)
    assert "skill-99" in small and "topic number 99" in big and "topic number 99" not in small  # names stay, details go
    assert len(small) < len(big) - 5000
