import json

from agent.learn_import import collect, evidence, project_dirs


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
    tasks, hooks = collect(repo, projects)
    assert [t["reward"] for t in tasks] == [-1, 1, -1]
    assert tasks[0]["files"] == ["web/booking.tsx"]
    assert tasks[1]["files"] == ["admin/booking.tsx"]
    assert hooks == {"lint first": 1}

    text = evidence(tasks, hooks, ["use pnpm"])
    assert "(1x) lint first" in text and "Already known:\n- use pnpm" in text
    assert "Replies right after the agent got it wrong:\n- no that's the wrong page" in text
