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


def test_taste_goes_into_the_prompt_and_nested_taste_shows_on_touch(tmp_path):
    from config.loader import _instructions_for

    repo, home = tmp_path / "repo", tmp_path / "home"
    (repo / ".git").mkdir(parents=True)
    (repo / "AGENTS.md").write_text("Use pnpm for every script in this repo, never npm.")
    taste = repo / ".commandcode" / "taste"
    (taste / "ui").mkdir(parents=True)
    (taste / "taste.md").write_text("# Taste (Continuously Learned by [CommandCode][cmd])\n\n[cmd]: https://commandcode.ai/\n\n# UI\nSee [ui/taste.md]\n\n# Money\n- Money is integer paise. Confidence: 0.75\n")
    (taste / "ui" / "taste.md").write_text("# UI\n- Never use emojis in UI. Confidence: 0.85\n")
    web = repo / "apps" / "web" / ".commandcode" / "taste"
    web.mkdir(parents=True)
    (web / "taste.md").write_text("- Use a peel effect for loading screens. Confidence: 0.70\n")

    out = _instructions_for(repo, home)
    assert out.index("Use pnpm") < out.index("Learned taste") < out.index("integer paise") < out.index("Never use emojis")
    assert "Continuously Learned" not in out and "[cmd]" not in out and "See [ui" not in out
    assert "peel effect" not in out  # a subfolder's taste waits until that folder is touched

    seen: set = set()
    shown = for_path(repo, repo / "apps" / "web" / "src" / "a.tsx", seen)
    assert "peel effect" in shown and "integer paise" not in shown  # the root's is already in the prompt
