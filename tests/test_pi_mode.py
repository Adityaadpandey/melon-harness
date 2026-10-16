from config.config import Config


def test_pi_mode_gives_the_seven_plain_tools_and_no_call_cap(tmp_path):
    c = Config(cwd=tmp_path)
    assert c.allowed_tools == ["read_file", "write_file", "edit", "shell", "grep", "glob", "list_dir", "graph", "todos", "subagent_codebase_investigator"]
    assert c.read_call_budget == 0 and not c.check_references and not c.auto_scout


def test_anything_set_by_hand_wins_over_pi_defaults(tmp_path):
    c = Config(cwd=tmp_path, read_call_budget=5, allowed_tools=["git"])
    assert c.read_call_budget == 5 and c.allowed_tools == ["git"]
    assert Config(cwd=tmp_path, pi_mode=False).allowed_tools is None


def test_pi_mode_compacts_only_near_the_window_and_still_clears_old_results(tmp_path):
    from context.manager import ContextManager

    cm = ContextManager(Config(cwd=tmp_path), None, None)
    cm.context_window = 100_000
    assert cm._limit() == 100_000 - 16_384
    cm.add_assistant_message("", [{"id": "a", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}])
    cm.add_tool_result("a", "x" * 400_000)
    cm.add_assistant_message("", [{"id": "b", "type": "function", "function": {"name": "read_file", "arguments": "{}"}}])
    cm.add_tool_result("b", "y" * 40_000)  # the newest result stays whole; the older one is cleared
    assert cm.prune_tool_outputs() == 1
    assert "Re-run the tool" in cm._messages[1].content and "recall_result" not in cm._messages[1].content


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
    assert len(tools) == 9 and "- shell:" in prompt and "- git:" not in prompt and "remembered" not in prompt
    assert count_tokens(prompt, cfg.model_name) < 560  # was 400 before the step list and the searcher were added, 480 before the two guidelines on root cause and held-back work


def test_pi_cap_keeps_the_tail_of_shell_output_and_the_head_of_everything_else(tmp_path):
    from tools.base import ToolResult
    from tools.registry import ToolRegistry

    reg = ToolRegistry(Config(cwd=tmp_path))
    text = "\n".join(f"line{i}" for i in range(1, 3001))
    sh, rd = ToolResult.success_result(text), ToolResult.success_result(text)
    reg._cap_output(sh, "shell")
    reg._cap_output(rd, "grep")
    assert "line3000" in sh.output and "line1\n" not in sh.output and "Showing last " in sh.output and "of 3000 lines" in sh.output and len(sh.output) < 13_000
    assert "line1\n" in rd.output and "line3000" not in rd.output and "Showing first 2000 of 3000 lines" in rd.output
    small = ToolResult.success_result("a\nb")
    reg._cap_output(small, "shell")
    assert small.output == "a\nb"


def test_a_sub_agent_never_gets_the_searcher_and_a_profile_investigator_replaces_the_builtin(tmp_path):
    from config.config import SubagentSpec

    assert "subagent_codebase_investigator" not in Config(cwd=tmp_path, model_role="review").allowed_tools
    spec = SubagentSpec(name="investigator", description="d", goal_prompt="g", allowed_tools=["read_file", "grep"])
    assert Config(cwd=tmp_path, subagents=[spec]).allowed_tools[-1] == "subagent_investigator"
    writer = SubagentSpec(name="investigator", description="d", goal_prompt="g", allowed_tools=["read_file", "shell"])
    assert not any(t.startswith("subagent_") for t in Config(cwd=tmp_path, subagents=[writer]).allowed_tools)  # a searcher that can run commands is not read-only


def test_write_file_ends_a_new_file_with_a_newline_but_leaves_a_no_newline_file_alone(tmp_path):
    import asyncio

    from tools.base import ToolInvocation
    from tools.builtin.write_file import WriteFileTool

    tool = WriteFileTool(Config(cwd=tmp_path))
    run = lambda name, text: asyncio.run(tool.execute(ToolInvocation(params={"path": name, "content": text}, cwd=tmp_path)))
    run("a.tsx", "export {}")
    assert (tmp_path / "a.tsx").read_text() == "export {}\n"  # biome/prettier fail a file without it
    (tmp_path / "b.txt").write_text("old")  # this file never had one: keep it that way
    run("b.txt", "new")
    assert (tmp_path / "b.txt").read_text() == "new"


def test_machine_wide_skills_are_listed_by_name_and_the_project_s_own_keep_their_description(tmp_path):
    from agent.skills import Skill
    from prompts.system import _skill_rows

    mine, theirs = tmp_path / "skills" / "motion" / "SKILL.md", tmp_path.parent / "elsewhere" / "figma" / "SKILL.md"
    rows = _skill_rows([Skill("motion", "Audit the animations", "", path=mine), Skill("figma", "Work in Figma files", "", path=theirs)], (tmp_path,))
    assert "motion/SKILL.md: Audit the animations" in rows and "figma/SKILL.md" in rows and "Work in Figma" not in rows
