from agent.instructions import for_path


def test_nested_instructions_show_once_outermost_first(tmp_path):
    (tmp_path / "AGENTS.md").write_text("Use pnpm.")
    web = tmp_path / "apps" / "web"
    web.mkdir(parents=True)
    (web / "CLAUDE.md").write_text("Run check-types.\n@docs.md")
    (web / "docs.md").write_text("Pages live in (list).")
    seen: set = set()
    out = for_path(tmp_path, web / "src" / "a.ts", seen)
    assert out.index("Use pnpm.") < out.index("Run check-types.") and "Pages live in (list)." in out
    assert for_path(tmp_path, web / "src" / "b.ts", seen) == ""  # already shown


def test_import_outside_the_repo_is_left_alone(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    (tmp_path / "secret.md").write_text("TOP SECRET")
    (repo / "AGENTS.md").write_text("@../secret.md")
    assert "TOP SECRET" not in for_path(repo, repo / "a.py", set())


def test_file_outside_root_gets_nothing(tmp_path):
    (tmp_path / "AGENTS.md").write_text("x")
    assert for_path(tmp_path / "sub", tmp_path / "other.py", set()) == ""
