import json

from agent.profile import describe


def test_a_pnpm_monorepo_is_described_from_its_manifests(tmp_path):
    (tmp_path / "package.json").write_text(json.dumps({"dependencies": {"next": "1", "@prisma/client": "1"}, "devDependencies": {"typescript": "5"},
                                                       "scripts": {"test": "vitest", "lint": "eslint .", "dev": "next"}}))
    (tmp_path / "pnpm-lock.yaml").write_text("")
    (tmp_path / "turbo.json").write_text("{}")
    (tmp_path / "AGENTS.md").write_text("rules")
    out = describe(tmp_path)
    assert "TypeScript" in out and "Next.js" in out and "Prisma" in out and "turbo" in out
    assert "`pnpm test`" in out and "`pnpm run lint`" in out and "dev" not in out.split("Run:")[1].split("\n")[0]
    assert "AGENTS.md" in out


def test_python_and_make_and_an_empty_folder(tmp_path):
    assert describe(tmp_path) == ""
    (tmp_path / "pyproject.toml").write_text('dependencies = ["fastapi", "pytest"]\n[tool.ruff]\n')
    (tmp_path / "Makefile").write_text("test:\n\tpytest\nlint:\n\truff check\n")
    out = describe(tmp_path)
    assert "Python" in out and "FastAPI" in out and "`python3 -m pytest -q`" in out and "`ruff check`" in out
