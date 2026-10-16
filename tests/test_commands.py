from utils.templates import expand, load_commands, split_args


def test_claude_style_command_files(tmp_path):
    d = tmp_path / ".claude" / "commands"
    (d / "review").mkdir(parents=True)
    (d / "fix.md").write_text("---\ndescription: Fix an issue\n---\nFix issue $1 and add a test. Notes: $ARGUMENTS")
    (d / "review" / "security.md").write_text("Review $ARGUMENTS for security problems")
    found = load_commands((d,))
    assert found["fix"][1] == "Fix an issue"
    assert found["review:security"][1].startswith("Review")
    assert expand(found["fix"][0], split_args("42 quickly")) == "Fix issue 42 and add a test. Notes: 42 quickly"
