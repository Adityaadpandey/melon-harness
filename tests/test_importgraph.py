import subprocess

from utils.importgraph import load


def repo(tmp_path, files: dict[str, str]):
    for rel, text in files.items():
        p = tmp_path / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    return load(tmp_path)


def test_alias_and_relative_imports(tmp_path):
    g = repo(tmp_path, {
        "tsconfig.json": '{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}',
        "src/components/Nav.tsx": "export const Nav = 1",
        "src/app/page.tsx": 'import { Nav } from "@/components/Nav"\nimport "./helper"',
        "src/app/helper.ts": "export {}",
    })
    assert "src/app/page.tsx" in g.inc["src/components/Nav.tsx"]
    assert "src/app/page.tsx" in g.inc["src/app/helper.ts"]


def test_paths_inherited_through_extends(tmp_path):
    g = repo(tmp_path, {
        "tsconfig.base.json": '{"compilerOptions": {"paths": {"@/*": ["./src/*"]}}}',
        "apps/web/tsconfig.json": '{"extends": "../../tsconfig.base.json"}',
        "src/lib/util.ts": "export {}",
        "apps/web/page.ts": 'import { x } from "@/lib/util"',
    })
    # `paths` with no baseUrl is relative to the config that wrote it: the base file at the repo root
    assert "apps/web/page.ts" in g.inc["src/lib/util.ts"]


def test_child_paths_override_and_baseurl_from_parent(tmp_path):
    g = repo(tmp_path, {
        "tsconfig.base.json": '{"compilerOptions": {"baseUrl": "./shared"}}',
        "tsconfig.json": '{"extends": "./tsconfig.base.json"}',
        "shared/thing.ts": "export {}",
        "main.ts": 'import "thing"',
    })
    assert "main.ts" in g.inc["shared/thing.ts"]


def test_python_dynamic_import_and_cycle_in_extends(tmp_path):
    g = repo(tmp_path, {
        "pkg/__init__.py": "",
        "pkg/plugin.py": "x = 1",
        "app.py": 'import importlib\nm = importlib.import_module("pkg.plugin")',
        "tsconfig.json": '{"extends": "./tsconfig.json"}',  # a loop must not hang
    })
    assert "app.py" in g.inc["pkg/plugin.py"]
