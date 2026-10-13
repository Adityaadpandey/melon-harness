import asyncio
import subprocess
import time

import pytest

import agent.orchestrator as orch
from agent.events import AgentEvent, AgentEventType
from config.config import Config


def git(p, *a):
    return subprocess.run(["git", *a], cwd=p, check=True, capture_output=True, text=True).stdout


@pytest.fixture
def repo(tmp_path):
    git(tmp_path, "init", "-q")
    git(tmp_path, "config", "user.email", "t@t")
    git(tmp_path, "config", "user.name", "t")
    (tmp_path / "shared.py").write_text("def a():\n    return 0\n\n\n\n\n\ndef b():\n    return 0\n")
    git(tmp_path, "add", "-A")
    git(tmp_path, "commit", "-qm", "init")
    return tmp_path


class FakeAgent:
    """Edits files according to a script keyed by a marker found in the prompt."""
    script = {}
    delay = 0.0

    def __init__(self, cfg, cb=None):
        self.cwd = cfg.cwd

    async def __aenter__(self):
        return self

    async def __aexit__(self, *a):
        pass

    async def run(self, prompt):
        await asyncio.sleep(self.delay)
        for marker, edits in self.script.items():
            if marker in prompt:
                for path, fn in edits.items():
                    f = self.cwd / path
                    f.write_text(fn(f.read_text() if f.exists() else ""))
        yield AgentEvent(AgentEventType.AGENT_END, {"report": {"status": "verified"}})


@pytest.fixture(autouse=True)
def fake(monkeypatch):
    monkeypatch.setattr(orch, "Agent", FakeAgent)
    FakeAgent.script, FakeAgent.delay = {}, 0.0


def cfg(repo):
    return Config(cwd=repo, auto_verify=False)


def test_graph_validation():
    g = lambda *t: {"tasks": [{"id": i, "goal": "g", "deps": d} for i, d in t]}
    assert [[n.id for n in w] for w in orch.waves(orch.parse_graph(g(("a", []), ("b", ["a"]), ("c", []))))] == [["a", "c"], ["b"]]
    with pytest.raises(orch.GraphError):
        orch.parse_graph(g(("a", ["b"]), ("b", ["a"])))
    with pytest.raises(orch.GraphError):
        orch.parse_graph(g(("a", ["zzz"])))
    with pytest.raises(orch.GraphError):
        orch.parse_graph(g(("a", []), ("a", [])))


async def test_independent_workers_run_concurrently_and_merge(repo):
    FakeAgent.delay = 0.4
    FakeAgent.script = {f"T{i}": {f"m{i}.py": lambda _: "x = 1\n"} for i in range(3)}
    nodes = [orch.Node(f"n{i}", f"T{i}") for i in range(3)]
    t = time.perf_counter()
    res = await orch.Orchestrator(cfg(repo)).run("req", nodes, apply=True)
    assert time.perf_counter() - t < 1.1  # serial would be >= 1.2s plus git overhead
    assert res["status"] == "ok" and res["applied"] == "fast-forwarded"
    assert all((repo / f"m{i}.py").exists() for i in range(3))
    # cleanup: no leftover worktrees or worker branches
    assert git(repo, "worktree", "list").count("\n") == 1
    assert "integration" in git(repo, "branch", "--list", "mew/*") and git(repo, "branch", "--list", "mew/*").count("\n") == 1


async def test_same_file_disjoint_hunks_merge_cleanly_and_are_reported(repo):
    FakeAgent.script = {
        "TA": {"shared.py": lambda s: s.replace("def a():\n    return 0", "def a():\n    return 1")},
        "TB": {"shared.py": lambda s: s.replace("def b():\n    return 0", "def b():\n    return 2")},
    }
    res = await orch.Orchestrator(cfg(repo)).run("r", [orch.Node("a", "TA"), orch.Node("b", "TB")], apply=True)
    assert res["status"] == "ok" and res["overlapping_files"] == {"shared.py": ["a", "b"]}
    text = (repo / "shared.py").read_text()
    assert "return 1" in text and "return 2" in text


async def test_true_conflict_is_detected_and_reconciled_by_rerun(repo):
    FakeAgent.script = {
        "TA": {"shared.py": lambda s: s.replace("def a():\n    return 0", "def a():\n    return 'A'")},
        "TB": {"shared.py": lambda s: s.replace("def a():\n    return 0", "def a():\n    return 'B'")
                                         if "'A'" not in s else s + "# saw A\n"},
    }
    res = await orch.Orchestrator(cfg(repo)).run("r", [orch.Node("a", "TA"), orch.Node("b", "TB")], apply=True)
    states = {t["id"]: t["status"] for t in res["tasks"]}
    assert states == {"a": "done", "b": "resolved"}
    assert "<<<<<<<" not in (repo / "shared.py").read_text()
    assert "review recommended" in next(t for t in res["tasks"] if t["id"] == "b")["note"]


async def test_conflict_without_resolver_is_reported_not_applied(repo):
    FakeAgent.script = {
        "TA": {"shared.py": lambda s: s.replace("return 0", "return 'A'", 1)},
        "TB": {"shared.py": lambda s: s.replace("return 0", "return 'B'", 1)},
    }
    res = await orch.Orchestrator(cfg(repo), resolve_conflicts=False).run(
        "r", [orch.Node("a", "TA"), orch.Node("b", "TB")], apply=True)
    assert res["status"] == "conflict" and res["applied"] == "not requested"
    assert "return 0" in (repo / "shared.py").read_text()  # user's tree untouched


async def test_dependent_task_sees_prerequisite_output(repo):
    seen = {}

    def second(s):
        seen["base"] = s
        return s

    FakeAgent.script = {"T1": {"one.txt": lambda _: "first"}, "T2": {"two.txt": lambda _: (repo / "x") and "second"}}
    nodes = [orch.Node("one", "T1"), orch.Node("two", "T2", deps=["one"])]
    orig = FakeAgent.run

    async def run(self, prompt):
        if "T2" in prompt:
            seen["one_exists"] = (self.cwd / "one.txt").exists()
        async for e in orig(self, prompt):
            yield e

    FakeAgent.run = run
    try:
        res = await orch.Orchestrator(cfg(repo)).run("r", nodes, apply=False)
    finally:
        FakeAgent.run = orig
    assert res["status"] == "ok" and seen["one_exists"] is True
    assert not (repo / "one.txt").exists()  # apply=False leaves the user's tree alone


async def test_apply_refuses_dirty_tree(repo):
    FakeAgent.script = {"T": {"new.py": lambda _: "1"}}
    (repo / "shared.py").write_text("user edits")
    res = await orch.Orchestrator(cfg(repo)).run("r", [orch.Node("a", "T")], apply=True)
    assert res["applied"].startswith("skipped") and (repo / "shared.py").read_text() == "user edits"


async def test_failed_dependency_skips_dependents(repo):
    class Boom(FakeAgent):
        async def run(self, prompt):
            raise RuntimeError("model died")
            yield

    orch.Agent = Boom
    res = await orch.Orchestrator(cfg(repo)).run("r", [orch.Node("a", "x"), orch.Node("b", "y", deps=["a"])])
    assert {t["id"]: t["status"] for t in res["tasks"]} == {"a": "failed", "b": "skipped"}


async def test_worker_that_changes_nothing_is_not_a_success(repo):
    FakeAgent.script = {}  # agent "finishes" without editing anything
    res = await orch.Orchestrator(cfg(repo)).run("r", [orch.Node("a", "T")], apply=True)
    assert res["status"] == "partial" and res["tasks"][0]["note"] == "worker made no changes"
