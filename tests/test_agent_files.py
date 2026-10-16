from config.config import Config
from tools.subagents import READONLY_TOOLS, get_default_subagent_definitions, parse_agent_file

FILE = """---
name: db-reviewer
description: Reviews SQL and migrations. Use after schema changes.
tools: Read, Grep, Bash
model: haiku
---
You review migrations for locks and data loss.
"""


def test_claude_code_agent_file_parses():
    d = parse_agent_file(FILE)
    assert (d.name, d.allowed_tools, d.role) == ("db_reviewer", ["read_file", "grep", "shell"], "fast")
    assert "locks" in d.goal_prompt


def test_file_without_name_or_body_is_skipped():
    assert parse_agent_file("---\ndescription: x\n---\nbody") is None
    assert parse_agent_file("---\nname: a\n---\n") is None


def test_repo_agents_join_the_built_ins(tmp_path):
    (tmp_path / ".claude" / "agents").mkdir(parents=True)
    (tmp_path / ".claude" / "agents" / "db.md").write_text(FILE)
    names = [d.name for d in get_default_subagent_definitions(Config(cwd=tmp_path))]
    assert {"codebase_investigator", "planner", "code_reviewer", "general", "db_reviewer"} <= set(names)


def test_planner_and_investigator_cannot_edit():
    for d in get_default_subagent_definitions():
        if d.name in ("planner", "codebase_investigator"):
            assert set(d.allowed_tools) <= READONLY_TOOLS
