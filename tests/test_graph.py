import json

import pytest

from config.config import Config
from tools.base import ToolInvocation
from tools.builtin.graph import GraphTool
from utils import codegraph


def n(i, label, f, loc, comm=1):
    return {"id": i, "label": label, "file_type": "code", "source_file": f"/Users/someone/old/proj/{f}", "source_location": loc, "community": comm}


def e(s, t, rel="calls", conf="EXTRACTED"):
    return {"source": s, "target": t, "relation": rel, "confidence": conf}


@pytest.fixture
def repo(tmp_path):
    (tmp_path / "src").mkdir()
    for f in ("cache.ts", "svc.ts", "team.ts"):
        (tmp_path / "src" / f).write_text("x")
    out = tmp_path / "graphify-out"
    out.mkdir()
    nodes = [
        n("cache_del", "delTicketTypeCache()", "src/cache.ts", "L46"),
        n("svc_upd", "updateTicketType()", "src/svc.ts", "L270", 2),
        n("svc_del", "deleteTicketType()", "src/svc.ts", "L340", 2),
        n("team_inc", "TeamBookingService.reserve()", "src/team.ts", "L223", 3),
        n("cache_file", "cache.ts", "src/cache.ts", "L1"),
    ]
    links = [e("svc_upd", "cache_del"), e("svc_del", "cache_del"), e("team_inc", "svc_upd", conf="INFERRED"), e("cache_file", "cache_del", "contains")]
    (out / "graph.json").write_text(json.dumps({"directed": False, "nodes": nodes, "links": links}))
    return tmp_path


async def ask(repo, **p):
    return await GraphTool(Config(cwd=repo)).execute(ToolInvocation(params=p, cwd=repo))


async def test_find_and_callers_map_graph_paths_onto_this_checkout(repo):
    r = await ask(repo, action="find", name="ticketype") if False else await ask(repo, action="find", name="TicketType")
    assert "delTicketTypeCache()" in r.output and "src/cache.ts:46" in r.output  # /Users/someone/old/proj/ prefix is stripped
    r = await ask(repo, action="callers", name="delTicketTypeCache")
    assert "called by 2" in r.output and "src/svc.ts:270" in r.output and "src/svc.ts:340" in r.output
    assert "contains" not in r.output  # only call edges


async def test_inferred_edges_are_marked_and_callees_path_community_work(repo):
    r = await ask(repo, action="callees", name="TeamBookingService.reserve")
    assert "~ calls" in r.output  # an inferred edge is visibly less certain
    r = await ask(repo, action="path", name="TeamBookingService.reserve", to="delTicketTypeCache")
    assert r.output.index("reserve") < r.output.index("updateTicketType") < r.output.index("delTicketTypeCache")
    r = await ask(repo, action="community", name="updateTicketType")
    assert "module 2 has 2 nodes" in r.output and "src/svc.ts" in r.output


async def test_unknown_names_and_missing_graph_fail_helpfully(repo, tmp_path_factory):
    assert "No graph node named" in (await ask(repo, action="callers", name="doesNotExist")).output
    assert not (await ask(repo, action="callers")).success
    empty = tmp_path_factory.mktemp("nograph")
    r = await ask(empty, action="find", name="x")
    assert not r.success and "grep" in r.error


def test_tool_is_only_registered_when_a_graph_exists(repo, tmp_path_factory):
    from tools.registry import create_default_registry

    assert create_default_registry(Config(cwd=repo)).get("graph") is not None
    assert create_default_registry(Config(cwd=tmp_path_factory.mktemp("plain"))).get("graph") is None
