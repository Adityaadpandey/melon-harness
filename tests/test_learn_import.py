import json

from agent.learn_import import chunks, collect, episodes, evidence, history_tasks, project_dirs, rulebook, story


def _rec(type_, content, **extra):
    return json.dumps({"type": type_, "message": {"content": content}, **extra})


def _edit(path):
    return _rec("assistant", [{"type": "tool_use", "name": "Edit", "input": {"file_path": path}}])


def test_rewards_files_and_hooks(tmp_path):
    repo = tmp_path / "shop"
    repo.mkdir()
    projects = tmp_path / "projects"
    folder = projects / str(repo).replace("/", "-")
    folder.mkdir(parents=True)
    (projects / (str(repo).replace("/", "-") + "x")).mkdir()  # "shopx" is another repo, not a subfolder
    lines = [
        _rec("user", "add a refund button to the booking page"),
        _edit(str(repo / "web/booking.tsx")),
        _edit("/elsewhere/scratch.py"),  # outside the repo: ignored
        _rec("user", "no that's the wrong page, use the admin one"),  # corrects the task before it
        _edit(str(repo / "admin/booking.tsx")),
        _rec("user", [{"type": "tool_result", "content": "PreToolUse:Bash hook error: lint first", "tool_use_id": "t"}],
             toolDenialKind="permission-rule"),
        _rec("user", "commit it in parts pls"),  # approves the admin task
        _edit(str(repo / "x.py")),
        _rec("user", [{"type": "text", "text": "[Request interrupted by user]"}]),  # punishes the commit task
        _rec("user", "<command-name>/clear</command-name>"),  # a slash command, not a task
        _rec("user", "subagent noise", isSidechain=True),
    ]
    (folder / "s.jsonl").write_text("\n".join(lines))

    assert project_dirs(repo, projects) == [folder]
    tasks, hooks, seen = collect(repo, projects)
    assert seen == {"s"}
    assert [t["reward"] for t in tasks] == [-1, 1, -1]
    assert tasks[0]["files"] == ["web/booking.tsx"]
    assert tasks[1]["files"] == ["admin/booking.tsx"]
    assert hooks == {"lint first": 1}

    text = evidence(tasks, hooks, ["use pnpm"])
    assert "(1x) lint first" in text and "Already known:\n- use pnpm" in text
    assert "Replies right after the agent got it wrong:\n- no that's the wrong page" in text


def test_deleted_sessions_come_back_from_prompt_history(tmp_path):
    repo = tmp_path / "shop"
    history = tmp_path / "history.jsonl"
    rows = [
        {"display": "add coupons to checkout", "timestamp": 1, "project": str(repo), "sessionId": "old"},
        {"display": "no, coupons go on the cart page", "timestamp": 2, "project": str(repo / "apps/web"), "sessionId": "old"},
        {"display": "/clear", "timestamp": 3, "project": str(repo), "sessionId": "old"},  # a slash command is not a task
        {"display": "kept transcript", "timestamp": 4, "project": str(repo), "sessionId": "s"},  # transcript still on disk
        {"display": "other repo", "timestamp": 5, "project": str(repo) + "x", "sessionId": "x"},
    ]
    history.write_text("\n".join(json.dumps(r) for r in rows))
    tasks = history_tasks(repo, history, skip={"s"})
    assert [(t["request"], t["reward"]) for t in tasks] == [("add coupons to checkout", -1), ("no, coupons go on the cart page", 0)]
    assert "YOU NEXT: no, coupons go on the cart page" in story(tasks[0])  # a punished task shows what you said next


def test_story_names_failed_commands_and_chunks_split(tmp_path):
    repo = tmp_path / "shop"
    lines = [
        _rec("user", "run the server tests"),
        _rec("assistant", [{"type": "tool_use", "id": "b1", "name": "Bash", "input": {"command": "npx vitest run"}}]),
        _rec("user", [{"type": "tool_result", "tool_use_id": "b1", "is_error": True, "content": "vitest: command not found"}]),
        _rec("assistant", [{"type": "text", "text": "vitest is missing, switching to pnpm test"}]),
        _rec("user", "never use vitest here, it's playwright only"),
    ]
    tasks, _ = episodes(lines, repo)
    s = story(tasks[0])
    assert "failed: npx vitest run -> vitest: command not found" in s and "CLAUDE: vitest is missing" in s
    assert len(chunks(tasks * 50, size=500)) > 1 and all(len(c) <= 1000 for c in chunks(tasks * 50, size=500))


def test_rulebook_skips_mews_own_section(tmp_path):
    (tmp_path / "apps/web").mkdir(parents=True)
    (tmp_path / "AGENTS.md").write_text("# Rules\nNever read .env\n\n## Learned by melon\n- old lesson under review\n")
    (tmp_path / "apps/web/AGENTS.md").write_text("Ask before UI changes")
    (tmp_path / ".claude/skills/debug").mkdir(parents=True)
    (tmp_path / ".claude/skills/debug/SKILL.md").write_text("---\nname: debug\ndescription: Use for any 500\n---\n")
    book = rulebook(tmp_path)
    assert "Never read .env" in book and "Ask before UI changes" in book and "Use for any 500" in book
    assert "old lesson under review" not in book


def test_apply_writes_the_reviewed_dry_run_without_a_model(tmp_path, monkeypatch):
    import agent.learn_import as li
    from agent.learn import Learned

    repo, base = tmp_path / "shop", tmp_path / "data"
    repo.mkdir()
    monkeypatch.setattr(li, "Learned", lambda root: Learned(root, base))
    L = Learned(repo, base)
    L.add_lesson("this lesson is noise and goes")
    L.add_lesson("this lesson is fine and stays")
    saved = L.path.with_suffix(".deep.json")
    saved.write_text(json.dumps({"drop": ["this lesson is noise and goes"], "add": ["Quote bracketed Next.js paths in shell commands"],
                                 "why": {}, "candidates": []}))
    monkeypatch.setattr(li, "deep", lambda *a, **k: (_ for _ in ()).throw(AssertionError("no model call on apply")))
    li.main(["--repo", str(repo), "--projects", str(tmp_path / "none"), "--history", str(tmp_path / "none.jsonl"), "--deep", "--apply"])
    assert Learned(repo, base).lessons() == ["this lesson is fine and stays", "Quote bracketed Next.js paths in shell commands"]
    agents = (repo / "AGENTS.md").read_text()
    assert "noise" not in agents and "- Quote bracketed Next.js paths" in agents
    assert not saved.exists()  # used up, so the next run starts fresh
