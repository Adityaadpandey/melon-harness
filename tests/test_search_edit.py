import subprocess

import pytest

from config.config import Config
from tools.base import ToolInvocation
from tools.builtin.edit_file import EditTool
from tools.builtin.glob import GlobTool
from tools.builtin.grep import GrepTool
from utils.edits import plan_edits


def git(p, *a):
    subprocess.run(["git", *a], cwd=p, check=True, capture_output=True)


@pytest.fixture
def repo(tmp_path):
    git(tmp_path, "init", "-q")
    (tmp_path / ".gitignore").write_text("ignored/\n*.log\n")
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "a.py").write_text("def load_user():\n    return 1\n\nclass Cache: pass\n")
    (tmp_path / "src" / "b.ts").write_text("export const loadUser = 1;\n")
    (tmp_path / ".github").mkdir()
    (tmp_path / ".github" / "ci.yml").write_text("run: load_user\n")
    (tmp_path / "ignored").mkdir()
    (tmp_path / "ignored" / "x.py").write_text("load_user secret\n")
    (tmp_path / "debug.log").write_text("load_user\n")
    (tmp_path / ".env").write_text("TOKEN=load_user\n")
    (tmp_path / "node_modules").mkdir()
    (tmp_path / "node_modules" / "m.js").write_text("load_user\n")
    return tmp_path


async def run(tool, cwd, **params):
    return await tool.execute(ToolInvocation(params=params, cwd=cwd))


async def test_grep_respects_gitignore_finds_dotdirs_and_skips_secrets(repo):
    r = await run(GrepTool(Config(cwd=repo)), repo, pattern="load_user", files_only=True)
    files = {line.split(" ")[0] for line in r.output.splitlines()}
    assert files == {"src/a.py", ".github/ci.yml"}  # not ignored/, *.log, .env, node_modules


async def test_grep_smart_case_context_glob_and_limit(repo):
    t = GrepTool(Config(cwd=repo))
    assert "src/b.ts:1:" in (await run(t, repo, pattern="loaduser|loadUser")).output  # lowercase+alt: smart case on
    r = await run(t, repo, pattern="Cache")  # has uppercase: case-sensitive
    assert "src/a.py:4:class Cache" in r.output
    r = await run(t, repo, pattern="return", context=1)
    assert "src/a.py-1-def load_user" in r.output and "src/a.py:2:    return 1" in r.output
    r = await run(t, repo, pattern="load", glob="*.ts")
    assert "b.ts" in r.output and "a.py" not in r.output
    (repo / "many.txt").write_text("hit\n" * 30)
    r = await run(t, repo, pattern="hit", limit=5)
    assert r.output.count("many.txt:") == 5 and "25 more matches" in r.output


async def test_grep_too_broad_lists_files_and_missing_path_suggests_real_one(repo):
    for i in range(20):
        (repo / f"f{i}.txt").write_text("hit\n" * 6)
    r = await run(GrepTool(Config(cwd=repo)), repo, pattern="hit", limit=10)
    assert "too broad" in r.output and "f3.txt" in r.output and "hit\n" not in r.output
    r = await run(GrepTool(Config(cwd=repo)), repo, pattern="x", path="nope/a.py")
    assert r.success is False and "src/a.py" in r.error


async def test_grep_literal_and_bad_regex(repo):
    t = GrepTool(Config(cwd=repo))
    assert (await run(t, repo, pattern="(", literal=True)).success is True
    r = await run(t, repo, pattern="(")
    assert not r.success and "literal=true" in r.error


async def test_glob_uses_gitignore_and_path_patterns(repo):
    t = GlobTool(Config(cwd=repo))
    assert (await run(t, repo, pattern="*.py")).output == "src/a.py"
    assert (await run(t, repo, pattern="src/**/*.ts")).output == "src/b.ts"
    assert ".github/ci.yml" in (await run(t, repo, pattern=".github/*.yml")).output


def test_plan_exact_multi_and_overlap():
    src = "a = 1\nb = 2\nc = 3\n"
    new, err, n = plan_edits(src, [("a = 1", "a = 10"), ("c = 3", "c = 30")])
    assert err is None and n == 2 and new == "a = 10\nb = 2\nc = 30\n"
    _, err, _ = plan_edits(src, [("a = 1\nb = 2", "x"), ("b = 2\nc = 3", "y")])
    assert "overlap" in err


def test_plan_reports_all_locations_when_not_unique():
    _, err, _ = plan_edits("x\ny\nx\n", [("x", "z")])
    assert "2 times (lines 1, 3)" in err
    new, _, n = plan_edits("x\ny\nx\n", [("x", "z")], replace_all=True)
    assert new == "z\ny\nz\n" and n == 2


def test_plan_fuzzy_trailing_space_and_smart_quotes():
    src = 'say("hello")   \nother\n'
    new, err, _ = plan_edits(src, [("say(“hello”)\n", 'say("bye")\n')])
    assert err is None and new == 'say("bye")\nother\n'


def test_plan_preserves_crlf():
    new, err, _ = plan_edits("a\r\nb\r\n", [("a", "A")])
    assert err is None and new == "A\r\nb\r\n"


def test_plan_not_found_gives_closest_lines():
    _, err, _ = plan_edits("def load_user(id):\n    pass\n", [("def load_user(uid):", "x")])
    assert "not found" in err and "def load_user(id)" in err


async def test_edit_tool_multi_edit_crlf_and_no_approval_for_failed_edit(tmp_path):
    f = tmp_path / "f.txt"
    f.write_bytes(b"one\r\ntwo\r\nthree\r\n")
    t = EditTool(Config(cwd=tmp_path))
    inv = ToolInvocation(params={"path": "f.txt", "edits": [{"old_string": "one", "new_string": "1"}, {"old_string": "three", "new_string": "3"}]}, cwd=tmp_path)
    conf = await t.get_confirmation(inv)
    assert conf and "-one" in conf.diff.to_diff()
    r = await t.execute(inv)
    assert r.success and f.read_bytes() == b"1\r\ntwo\r\n3\r\n"
    bad = ToolInvocation(params={"path": "f.txt", "old_string": "nope", "new_string": "x"}, cwd=tmp_path)
    assert await t.get_confirmation(bad) is None  # nothing to approve
    assert not (await t.execute(bad)).success


async def test_edit_creates_new_file_with_empty_old_string(tmp_path):
    t = EditTool(Config(cwd=tmp_path))
    r = await t.execute(ToolInvocation(params={"path": "sub/new.py", "new_string": "x = 1\n"}, cwd=tmp_path))
    assert r.success and (tmp_path / "sub" / "new.py").read_text() == "x = 1\n"


async def test_search_ranks_files_by_plain_words(repo):
    from tools.builtin.search import SearchTool

    (repo / "src" / "EventAbout.tsx").write_text("// blank lines vanish on render\nexport const EventAbout = 1;\n")
    (repo / "src" / "unrelated.py").write_text("def other():\n    return 'cache'\n")
    (repo / "docs").mkdir()
    (repo / "docs" / "notes.md").write_text("event about blank lines render\n" * 5)
    r = await run(SearchTool(Config(cwd=repo)), repo, query="event about blank lines")
    files = [l.split()[0] for l in r.output.splitlines() if not l.startswith(" ")]
    assert files[0] == "src/EventAbout.tsx"
    assert files.index("src/EventAbout.tsx") < files.index("docs/notes.md")
