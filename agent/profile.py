"""A few lines about the repo the agent is standing in, read from its own manifests, never guessed.

Example for a pnpm monorepo:
    # This repo
    - Stack: TypeScript, Next.js, Prisma; pnpm workspace + turbo
    - Run: test `pnpm test`, lint `pnpm lint`, typecheck `pnpm typecheck`
    - Rules live in: AGENTS.md, CONTRIBUTING.md; CI in .github/workflows

The model otherwise spends its first calls finding out which package manager and test command the repo uses."""
from __future__ import annotations

import json
import re
from pathlib import Path

from agent.verify import _package_runner

JS_FRAMEWORKS = {
    "next": "Next.js", "react": "React", "vue": "Vue", "svelte": "Svelte", "expo": "Expo", "express": "Express",
    "fastify": "Fastify", "@nestjs/core": "NestJS", "prisma": "Prisma", "@prisma/client": "Prisma", "drizzle-orm": "Drizzle",
    "vitest": "Vitest", "jest": "Jest", "playwright": "Playwright", "@playwright/test": "Playwright",
}
PY_FRAMEWORKS = {"django": "Django", "fastapi": "FastAPI", "flask": "Flask", "sqlalchemy": "SQLAlchemy", "pytest": "pytest", "pydantic": "Pydantic"}
SCRIPTS = ("test", "lint", "typecheck", "check")  # no build: a build is the user's to run, listing it invites the slow call
RULE_FILES = ("AGENTS.md", "CLAUDE.md", "CONTRIBUTING.md")
MONOREPO = (("pnpm-workspace.yaml", "pnpm workspace"), ("turbo.json", "turbo"), ("nx.json", "nx"), ("lerna.json", "lerna"))


def _read(p: Path) -> str:
    try:
        return p.read_text(errors="replace")[:60_000]
    except OSError:
        return ""


def _js(root: Path, stack: list[str], run: list[str], tools: list[str]) -> None:
    try:
        pkg = json.loads(_read(root / "package.json"))
    except ValueError:
        return
    deps = {**pkg.get("devDependencies", {}), **pkg.get("dependencies", {})}
    stack.append("TypeScript" if "typescript" in deps or (root / "tsconfig.json").exists() else "JavaScript")
    tools += [name for dep, name in JS_FRAMEWORKS.items() if dep in deps]
    runner = _package_runner(root, root)
    scripts = pkg.get("scripts", {})
    run += [f"{s} `{runner} {'test' if s == 'test' else 'run ' + s}`" for s in SCRIPTS if s in scripts]
    if pkg.get("workspaces"):
        tools.append(f"{runner} workspaces")


def _py(root: Path, stack: list[str], run: list[str], tools: list[str]) -> None:
    text = _read(root / "pyproject.toml") + _read(root / "requirements.txt") + _read(root / "setup.py")
    if not text and not (root / "tests").is_dir():
        return
    stack.append("Python")
    low = text.lower()
    tools += [name for dep, name in PY_FRAMEWORKS.items() if re.search(rf"(^|[\s\"'\[])\s*{dep}\b", low)]
    if "pytest" in low or (root / "tests").is_dir() or (root / "pytest.ini").exists():
        run.append("test `python3 -m pytest -q`")
    if "ruff" in low:
        run.append("lint `ruff check`")


def _make(root: Path, run: list[str]) -> None:
    targets = set(re.findall(r"^([a-z][\w-]*):", _read(root / "Makefile"), re.M))
    run += [f"{t} `make {t}`" for t in ("test", "lint", "check") if t in targets and not any(r.startswith(t + " ") for r in run)]


def describe(cwd: Path) -> str:
    """The section, or "" when nothing recognisable is there (an empty folder, a folder of repos)."""
    root = Path(cwd)
    stack: list[str] = []
    run: list[str] = []
    tools: list[str] = []
    if (root / "package.json").is_file():
        _js(root, stack, run, tools)
    _py(root, stack, run, tools)
    for marker, lang, cmd in (("go.mod", "Go", "test `go test ./...`"), ("Cargo.toml", "Rust", "test `cargo test`"),
                              ("Gemfile", "Ruby", ""), ("pom.xml", "Java (Maven)", "test `mvn test`"), ("composer.json", "PHP", "")):
        if (root / marker).is_file():
            stack.append(lang)
            run += [cmd] if cmd else []
    _make(root, run)
    tools += [label for f, label in MONOREPO if (root / f).exists() and label not in tools]
    if not stack and not run:
        return ""
    rules = [f for f in RULE_FILES if (root / f).is_file()]
    lines = ["# This repo", f"- Stack: {', '.join(dict.fromkeys(stack))}" + (f"; {', '.join(dict.fromkeys(tools))}" if tools else "")]
    if run:
        lines.append(f"- Run: {', '.join(run)}")
    if rules or (root / ".github" / "workflows").is_dir():
        lines.append("- " + "; ".join(x for x in (f"Rules in {', '.join(rules)}" if rules else "", "CI in .github/workflows" if (root / ".github" / "workflows").is_dir() else "") if x))
    lines.append("Read from the manifests, so trust these over guessing; the package's own scripts win in a monorepo sub-folder.")
    return "\n".join(lines)
