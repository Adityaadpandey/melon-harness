//! Ported from tests/test_profile.py.
use crate::agent::profile::describe;
use serde_json::json;

#[test]
fn a_pnpm_monorepo_is_described_from_its_manifests() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let pkg = json!({"dependencies": {"next": "1", "@prisma/client": "1"}, "devDependencies": {"typescript": "5"},
                     "scripts": {"test": "vitest", "lint": "eslint .", "dev": "next"}});
    std::fs::write(d.join("package.json"), pkg.to_string()).unwrap();
    std::fs::write(d.join("pnpm-lock.yaml"), "").unwrap();
    std::fs::write(d.join("turbo.json"), "{}").unwrap();
    std::fs::write(d.join("AGENTS.md"), "rules").unwrap();
    let out = describe(d).unwrap();
    assert!(out.contains("TypeScript") && out.contains("Next.js") && out.contains("Prisma") && out.contains("turbo"), "{out}");
    assert!(out.contains("`pnpm test`") && out.contains("`pnpm run lint`"), "{out}");
    // the dev server is not offered as a command to run
    let run_line = out.split("Run:").nth(1).unwrap().split('\n').next().unwrap();
    assert!(!run_line.contains("dev"), "{run_line}");
    assert!(out.contains("AGENTS.md"));
}

#[test]
fn python_and_make_and_an_empty_folder() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    assert_eq!(describe(d).unwrap_or_default(), "");
    std::fs::write(d.join("pyproject.toml"), "dependencies = [\"fastapi\", \"pytest\"]\n[tool.ruff]\n").unwrap();
    std::fs::write(d.join("Makefile"), "test:\n\tpytest\nlint:\n\truff check\n").unwrap();
    let out = describe(d).unwrap();
    assert!(out.contains("Python") && out.contains("FastAPI") && out.contains("`python3 -m pytest -q`") && out.contains("`ruff check`"), "{out}");
}
