//! Ported from tests/test_importgraph.py: tsconfig aliases, `extends` chains and Python dynamic imports feed the
//! live import graph.
use super::git;
use crate::util::importgraph::{load, ImportGraph};
use std::fs;
use std::path::Path;
use std::sync::Arc;

fn repo(p: &Path, files: &[(&str, &str)]) -> Arc<ImportGraph> {
    for (rel, text) in files {
        let f = p.join(rel);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, text).unwrap();
    }
    git(p, &["init", "-q"]);
    load(p)
}

fn imported_by(g: &ImportGraph, file: &str, importer: &str) -> bool {
    g.inc.get(file).map_or(false, |s| s.contains(importer))
}

#[test]
fn alias_and_relative_imports() {
    let d = tempfile::tempdir().unwrap();
    let g = repo(d.path(), &[
        ("tsconfig.json", r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}"#),
        ("src/components/Nav.tsx", "export const Nav = 1"),
        ("src/app/page.tsx", "import { Nav } from \"@/components/Nav\"\nimport \"./helper\""),
        ("src/app/helper.ts", "export {}"),
    ]);
    assert!(imported_by(&g, "src/components/Nav.tsx", "src/app/page.tsx"));
    assert!(imported_by(&g, "src/app/helper.ts", "src/app/page.tsx"));
}

#[test]
fn paths_inherited_through_extends() {
    let d = tempfile::tempdir().unwrap();
    let g = repo(d.path(), &[
        ("tsconfig.base.json", r#"{"compilerOptions": {"paths": {"@/*": ["./src/*"]}}}"#),
        ("apps/web/tsconfig.json", r#"{"extends": "../../tsconfig.base.json"}"#),
        ("src/lib/util.ts", "export {}"),
        ("apps/web/page.ts", "import { x } from \"@/lib/util\""),
    ]);
    // `paths` with no baseUrl is relative to the config that wrote it: the base file at the repo root
    assert!(imported_by(&g, "src/lib/util.ts", "apps/web/page.ts"));
}

#[test]
fn child_paths_override_and_baseurl_from_parent() {
    let d = tempfile::tempdir().unwrap();
    let g = repo(d.path(), &[
        ("tsconfig.base.json", r#"{"compilerOptions": {"baseUrl": "./shared"}}"#),
        ("tsconfig.json", r#"{"extends": "./tsconfig.base.json"}"#),
        ("shared/thing.ts", "export {}"),
        ("main.ts", "import \"thing\""),
    ]);
    assert!(imported_by(&g, "shared/thing.ts", "main.ts"));
}

#[test]
fn python_dynamic_import_and_cycle_in_extends() {
    let d = tempfile::tempdir().unwrap();
    let g = repo(d.path(), &[
        ("pkg/__init__.py", ""),
        ("pkg/plugin.py", "x = 1"),
        ("app.py", "import importlib\nm = importlib.import_module(\"pkg.plugin\")"),
        // a loop must not hang
        ("tsconfig.json", r#"{"extends": "./tsconfig.json"}"#),
    ]);
    assert!(imported_by(&g, "pkg/plugin.py", "app.py"));
}
