import json

from agent.verify import detect_js_checks


def pkg(d, scripts):
    d.mkdir(parents=True, exist_ok=True)
    (d / "package.json").write_text(json.dumps({"scripts": scripts}))


def test_typecheck_script_of_the_changed_package(tmp_path):
    (tmp_path / "node_modules").mkdir()
    (tmp_path / "pnpm-lock.yaml").write_text("")
    pkg(tmp_path, {"build": "turbo build"})
    pkg(tmp_path / "apps/web", {"check-types": "tsc --noEmit", "lint": "next lint"})
    assert detect_js_checks(tmp_path, ["apps/web/src/a.tsx", "apps/web/src/b.ts"]) == [("pnpm run check-types", "apps/web")]


def test_nothing_when_not_installed_or_script_is_unsafe(tmp_path):
    pkg(tmp_path, {"typecheck": "tsc --noEmit"})
    assert detect_js_checks(tmp_path, ["a.ts"]) == []  # no node_modules
    (tmp_path / "node_modules").mkdir()
    pkg(tmp_path, {"typecheck": "prisma generate && tsc"})
    assert detect_js_checks(tmp_path, ["a.ts"]) == []
    pkg(tmp_path, {"typecheck": "tsc --noEmit"})
    assert detect_js_checks(tmp_path, ["README.md"]) == []
    assert detect_js_checks(tmp_path, ["a.ts"]) == [("npm run typecheck", ".")]
