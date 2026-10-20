from agent.verify import detect_checks


def touch(p, text="x"):
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)


def test_only_the_changed_files_are_checked_never_a_build_or_the_whole_suite(tmp_path):
    touch(tmp_path / "pnpm-lock.yaml", "")
    touch(tmp_path / "package.json", '{"scripts": {"build": "turbo build", "typecheck": "tsc --noEmit"}}')
    touch(tmp_path / "apps/web/package.json", '{"scripts": {"check-types": "tsc --noEmit"}}')
    touch(tmp_path / "node_modules/.bin/eslint")
    touch(tmp_path / "apps/web/src/a.tsx")
    touch(tmp_path / "pkg/store.py")
    touch(tmp_path / "tests/test_store.py")
    touch(tmp_path / "pkg/util.py")
    assert detect_checks(tmp_path, ["apps/web/src/a.tsx", "pkg/store.py", "README.md"]) == [
        ("python3 -m pytest -q -x tests/test_store.py", "."),
        ("pnpm exec eslint --quiet src/a.tsx", "apps/web"),
    ]
    assert detect_checks(tmp_path, ["pkg/util.py"]) == [("python3 -m py_compile pkg/util.py", ".")]  # no test for it: a syntax check


def test_nothing_for_files_with_no_cheap_check(tmp_path):
    # a Swift app's Tests/ folder is tests/ on macOS: it once made melon run pytest on Swift and call "no tests ran" a failure
    touch(tmp_path / "Tests/check.swift")
    touch(tmp_path / "Sources/main.swift")
    touch(tmp_path / "web/a.ts")  # eslint not installed
    assert detect_checks(tmp_path, ["Sources/main.swift", "web/a.ts", "gone.py"]) == []
