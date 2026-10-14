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
    (tmp_path / "src" / "cache.ts").write_text("export function delTicketTypeCache(id) {\n  return cacheDel(buildKey(id));\n}\n")
    (tmp_path / "src" / "svc.ts").write_text(
        "class S {\n  async updateTicketType(t) {\n    await delTicketTypeCache(t.eventId);\n  }\n"
        "  async deleteTicketType(t) {\n    await delTicketTypeCache(t.eventId);\n  }\n}\n"
    )
    (tmp_path / "src" / "team.ts").write_text("class T {\n  async reserve() {\n    await this.updateTicketType();\n  }\n}\n")
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
    assert "2 that call it" in r.output and "src/svc.ts:270" in r.output and "src/svc.ts:340" in r.output
    assert "cache.ts" not in r.output.split("\n", 2)[2]  # a 'contains' edge is not a call


async def test_inferred_edges_are_marked_and_callees_path_community_work(repo):
    r = await ask(repo, action="callees", name="TeamBookingService.reserve")
    assert "1 that it calls" in r.output and "~ " in r.output and "updateTicketType" in r.output  # inferred edge is marked
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


async def test_direction_comes_from_the_source_text_not_the_graphs_edge_order(repo):
    """The graph stores undirected edges in arbitrary order. Flip one and the answer must not change."""
    import json
    f = repo / "graphify-out" / "graph.json"
    g = json.loads(f.read_text())
    for e in g["links"]:
        e["source"], e["target"] = e["target"], e["source"]
    f.write_text(json.dumps(g))
    codegraph._CACHE.clear()
    r = await ask(repo, action="callers", name="delTicketTypeCache")
    assert "2 that call it" in r.output
    r = await ask(repo, action="callees", name="delTicketTypeCache")
    assert "0 that it calls" in r.output  # cache.ts calls cacheDel/buildKey, which are not in this graph


async def test_impact_lists_files_that_really_call_into_the_changed_file(repo):
    r = await ask(repo, action="impact", name="src/cache.ts")
    assert "src/svc.ts" in r.output and "src/team.ts" not in r.output  # team.ts only reaches cache.ts through svc.ts
