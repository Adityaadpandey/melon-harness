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


async def test_outline_and_read_symbol_return_one_function(repo):
    from tools.builtin.outline import OutlineTool
    from tools.builtin.read_file import ReadFileTool

    (repo / "big.ts").write_text(
        "export function alpha() {\n  return 1;\n}\n\nexport const beta = async (x: number) => {\n  return x;\n};\n\n"
        "class Box {\n  size() {\n    return 2;\n  }\n}\n"
    )
    (repo / "m.py").write_text("class A:\n    def f(self):\n        return 1\n\ndef g():\n    return 2\n")
    out = (await run(OutlineTool(Config(cwd=repo)), repo, path="big.ts")).output
    assert "1-3" in out and "function alpha" in out and "const beta" in out and "method size" in out
    assert "def f" in (await run(OutlineTool(Config(cwd=repo)), repo, path="m.py")).output
    r = await run(ReadFileTool(Config(cwd=repo)), repo, path="big.ts", symbol="beta")
    assert "return x;" in r.output and "alpha" not in r.output
    r = await run(ReadFileTool(Config(cwd=repo)), repo, path="big.ts", symbol="nope")
    assert not r.success and "outline" in r.error


async def test_git_tool_is_read_only_and_finds_changes(repo):
    from tools.builtin.git import GitTool

    git(repo, "add", "-A")
    git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "add loader")
    t = GitTool(Config(cwd=repo))
    assert "add loader" in (await run(t, repo, op="log", path="src/a.py")).output
    assert "add loader" in (await run(t, repo, op="find_change", query="load_user")).output
    assert "load_user" in (await run(t, repo, op="blame", path="src/a.py", lines="1,1")).output
    assert not (await run(t, repo, op="log", rev="--output=x")).success
    from pydantic import ValidationError
    with pytest.raises(ValidationError):
        await run(t, repo, op="commit")


def test_outline_maps_jsx_sections_inside_a_big_component():
    from utils.outline import outline

    src = "export default function Page() {\n  return (\n    <div>\n      {/* About the Event */}\n      {x && (\n        <A />\n      )}\n\n      {/* Footer */}\n      <B />\n    </div>\n  );\n}\n"
    names = {s.name: (s.start, s.end) for s in outline("Page.tsx", src)}
    assert names["Page"] == (1, 13) and names["About the Event"] == (4, 7) and names["Footer"][0] == 9


def test_atomic_write_keeps_mode_crlf_and_leaves_no_temp_files(tmp_path):
    import os, stat
    from utils.paths import atomic_write

    f = tmp_path / "x.sh"
    f.write_bytes(b"old\r\n")
    os.chmod(f, 0o755)
    atomic_write(f, "new\r\nline\r\n")
    assert f.read_bytes() == b"new\r\nline\r\n" and stat.S_IMODE(f.stat().st_mode) == 0o755
    assert [p.name for p in tmp_path.iterdir()] == ["x.sh"]


async def test_git_scope_reports_branch_uncommitted_and_branch_commits(tmp_path):
    import subprocess
    from tools.builtin.git import GitTool
    from tools.base import ToolInvocation

    def g(*a):
        subprocess.run(["git", *a], cwd=tmp_path, check=True, capture_output=True)

    g("init", "-q", "-b", "main"); g("config", "user.email", "a@b"); g("config", "user.name", "t")
    (tmp_path / "a.txt").write_text("1\n"); g("add", "."); g("commit", "-qm", "base")
    g("checkout", "-qb", "feature")
    (tmp_path / "b.txt").write_text("2\n"); g("add", "."); g("commit", "-qm", "add b")
    (tmp_path / "a.txt").write_text("changed\n")
    out = (await GitTool(Config(cwd=tmp_path)).execute(ToolInvocation(params={"op": "scope"}, cwd=tmp_path))).output
    assert "branch: feature" in out and "add b" in out and "a.txt" in out and "b.txt" in out


async def test_git_patch_returns_every_changed_file_in_one_call(tmp_path):
    import subprocess
    from tools.base import ToolInvocation
    from tools.builtin.git import GitTool
    from config.config import Config

    def sh(*a):
        subprocess.run(a, cwd=tmp_path, check=True, capture_output=True)

    sh("git", "init", "-q"); sh("git", "config", "user.email", "t@t"); sh("git", "config", "user.name", "t")
    for n in ("a.txt", "b.txt"):
        (tmp_path / n).write_text("one\n")
    sh("git", "add", "."); sh("git", "commit", "-qm", "init")
    (tmp_path / "a.txt").write_text("one\ntwo\n")
    (tmp_path / "b.txt").write_text("changed\n")
    res = await GitTool(Config(cwd=tmp_path)).execute(ToolInvocation(params={"op": "patch"}, cwd=tmp_path))
    assert res.success and "+two" in res.output and "+changed" in res.output


async def test_git_patch_keeps_paths_with_spaces_as_separate_files(tmp_path):
    import subprocess
    from tools.base import ToolInvocation
    from tools.builtin.git import GitTool
    from config.config import Config

    def sh(*a):
        subprocess.run(a, cwd=tmp_path, check=True, capture_output=True)

    sh("git", "init", "-q"); sh("git", "config", "user.email", "t@t"); sh("git", "config", "user.name", "t")
    for n in ("sp ace.txt", "b.txt"):
        (tmp_path / n).write_text("one\n")
    sh("git", "add", "."); sh("git", "commit", "-qm", "init")
    (tmp_path / "sp ace.txt").write_text("two\n")
    (tmp_path / "b.txt").write_text("changed\n")
    res = await GitTool(Config(cwd=tmp_path)).execute(ToolInvocation(params={"op": "patch"}, cwd=tmp_path))
    assert res.output.count("diff --git") == 2
