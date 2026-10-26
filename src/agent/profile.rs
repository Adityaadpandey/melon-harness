//! A few lines about the repo the agent is standing in, read from its own manifests, never guessed. The model otherwise
//! spends its first calls finding out which package manager and test command the repo uses.
use crate::agent::verify::package_runner;
use crate::util::re::{lit_i, Re};
use serde_json::Value;
use std::path::Path;

const JS_FRAMEWORKS: &[(&str, &str)] = &[
    ("next", "Next.js"), ("react", "React"), ("vue", "Vue"), ("svelte", "Svelte"), ("expo", "Expo"), ("express", "Express"), ("fastify", "Fastify"),
    ("@nestjs/core", "NestJS"), ("prisma", "Prisma"), ("@prisma/client", "Prisma"), ("drizzle-orm", "Drizzle"), ("vitest", "Vitest"), ("jest", "Jest"),
    ("playwright", "Playwright"), ("@playwright/test", "Playwright"),
];
const PY_FRAMEWORKS: &[(&str, &str)] = &[("django", "Django"), ("fastapi", "FastAPI"), ("flask", "Flask"), ("sqlalchemy", "SQLAlchemy"), ("pytest", "pytest"), ("pydantic", "Pydantic")];
/// No build: a build is the user's to run, listing it invites the slow call.
const SCRIPTS: &[&str] = &["test", "lint", "typecheck", "check"];
const RULE_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", "CONTRIBUTING.md"];
const MONOREPO: &[(&str, &str)] = &[("pnpm-workspace.yaml", "pnpm workspace"), ("turbo.json", "turbo"), ("nx.json", "nx"), ("lerna.json", "lerna")];

fn read(p: &Path) -> String {
    std::fs::read(p).map(|b| String::from_utf8_lossy(&b[..b.len().min(60_000)]).to_string()).unwrap_or_default()
}

fn js(root: &Path, stack: &mut Vec<String>, run: &mut Vec<String>, tools: &mut Vec<String>) {
    let Ok(pkg) = serde_json::from_str::<Value>(&read(&root.join("package.json"))) else { return };
    let has = |k: &str| pkg["devDependencies"].get(k).is_some() || pkg["dependencies"].get(k).is_some();
    stack.push(if has("typescript") || root.join("tsconfig.json").exists() { "TypeScript" } else { "JavaScript" }.into());
    for (dep, name) in JS_FRAMEWORKS {
        if has(dep) {
            tools.push(name.to_string());
        }
    }
    let runner = package_runner(root, root);
    for s in SCRIPTS {
        if pkg["scripts"].get(*s).is_some() {
            run.push(format!("{s} `{runner} {}`", if *s == "test" { "test".to_string() } else { format!("run {s}") }));
        }
    }
    if !pkg["workspaces"].is_null() {
        tools.push(format!("{runner} workspaces"));
    }
}

fn py(root: &Path, stack: &mut Vec<String>, run: &mut Vec<String>, tools: &mut Vec<String>) {
    let text = format!("{}{}{}", read(&root.join("pyproject.toml")), read(&root.join("requirements.txt")), read(&root.join("setup.py")));
    if text.is_empty() && !root.join("tests").is_dir() {
        return;
    }
    stack.push("Python".into());
    let low = text.to_lowercase();
    for (dep, name) in PY_FRAMEWORKS {
        if Re::new(&format!(r#"(^|[\s"'\[])\s*{dep}\b"#), false).map_or(false, |r| r.is_match(&low)) || low.lines().any(|l| l.trim_start().starts_with(dep) && !l.trim_start().starts_with('#')) {
            tools.push(name.to_string());
        }
    }
    if low.contains("pytest") || root.join("tests").is_dir() || root.join("pytest.ini").exists() {
        run.push("test `python3 -m pytest -q`".into());
    }
    if low.contains("ruff") {
        run.push("lint `ruff check`".into());
    }
}

fn make(root: &Path, run: &mut Vec<String>) {
    let text = read(&root.join("Makefile"));
    let targets: Vec<String> = text.lines().filter_map(|l| lit_i(r"^([a-z][\w-]*):").captures(l).and_then(|c| c[1].clone())).collect();
    for t in ["test", "lint", "check"] {
        if targets.iter().any(|x| x == t) && !run.iter().any(|r| r.starts_with(&format!("{t} "))) {
            run.push(format!("{t} `make {t}`"));
        }
    }
}

fn dedup(v: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for x in v {
        if !out.contains(x) {
            out.push(x.clone());
        }
    }
    out
}

/// The section, or None when nothing recognisable is there (an empty folder, a folder of repos).
pub fn describe(cwd: &Path) -> Option<String> {
    let (mut stack, mut run, mut tools): (Vec<String>, Vec<String>, Vec<String>) = (vec![], vec![], vec![]);
    if cwd.join("package.json").is_file() {
        js(cwd, &mut stack, &mut run, &mut tools);
    }
    py(cwd, &mut stack, &mut run, &mut tools);
    for (marker, lang, cmd) in [("go.mod", "Go", "test `go test ./...`"), ("Cargo.toml", "Rust", "test `cargo test`"), ("Gemfile", "Ruby", ""), ("pom.xml", "Java (Maven)", "test `mvn test`"), ("composer.json", "PHP", "")] {
        if cwd.join(marker).is_file() {
            stack.push(lang.into());
            if !cmd.is_empty() {
                run.push(cmd.into());
            }
        }
    }
    make(cwd, &mut run);
    for (f, label) in MONOREPO {
        if cwd.join(f).exists() && !tools.iter().any(|t| t == label) {
            tools.push(label.to_string());
        }
    }
    if stack.is_empty() && run.is_empty() {
        return None;
    }
    let rules: Vec<&str> = RULE_FILES.iter().copied().filter(|f| cwd.join(f).is_file()).collect();
    let ci = cwd.join(".github/workflows").is_dir();
    let mut lines = vec!["# This repo".to_string(), format!("- Stack: {}{}", dedup(&stack).join(", "), if tools.is_empty() { String::new() } else { format!("; {}", dedup(&tools).join(", ")) })];
    if !run.is_empty() {
        lines.push(format!("- Run: {}", run.join(", ")));
    }
    if !rules.is_empty() || ci {
        let parts: Vec<String> = [if rules.is_empty() { String::new() } else { format!("Rules in {}", rules.join(", ")) }, if ci { "CI in .github/workflows".into() } else { String::new() }].into_iter().filter(|x| !x.is_empty()).collect();
        lines.push(format!("- {}", parts.join("; ")));
    }
    lines.push("Read from the manifests, so trust these over guessing; the package's own scripts win in a monorepo sub-folder.".into());
    Some(lines.join("\n"))
}
