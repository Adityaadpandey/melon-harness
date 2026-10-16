from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from utils import codegraph, importgraph
from utils.codegraph import kind_of

LIMIT = 25


class GraphParams(BaseModel):
    action: str = Field(
        ...,
        description="find (locate symbols/files by name) | callers (who calls NAME; direction checked in source) | callees (what NAME calls) | "
        "importers (files that import FILE, live from the working tree; follows @/ aliases) | imports (what FILE imports) | blast (everything that depends on FILE, nearest first) | tests (tests that cover FILE) | "
        "neighbors (all relations of NAME) | impact (files that depend on file/symbol NAME: what a change could break) | path (shortest connection NAME -> TO) | community (the module NAME belongs to) | stats",
    )
    name: str | None = Field(None, description="Symbol, class or file name (e.g. delTicketTypeCache, ticket.service.ts, apps/web/src/components/ui/DesktopNav.tsx)")
    to: str | None = Field(None, description="Second name, for action=path")


class GraphTool(Tool):
    name = "graph"
    description = (
        "Understand how the code connects. importers/imports/blast/tests are computed live from the working tree (always current): "
        "who imports a file, what breaks if it changes, which tests cover it. find/callers/callees/path need a prebuilt graph snapshot "
        "(graphify-out/graph.json) and are pointers to verify with read_file. One call replaces several grep/read rounds."
    )
    kind = ToolKind.READ
    schema = GraphParams

    def _live(self, act: str, name: str | None, cwd) -> ToolResult:
        if not name:
            return ToolResult.error_result("`name` (a file path or file name) is required for this action")
        ig = importgraph.load(cwd)
        rel, maybe = ig.find_file(name)
        if rel is None:
            return ToolResult.success_result(f"No single file matches '{name}'." + (f" Did you mean: {', '.join(maybe)}" if maybe else " Use glob to find the path."))
        if act == "imports":
            rows = sorted(ig.out[rel])
            return ToolResult.success_result(f"{rel} imports {len(rows)} project file(s):\n" + "\n".join(f"  {r}" for r in rows[:LIMIT * 2]))
        if act == "importers":
            rows = sorted(ig.inc.get(rel, ()))
            return ToolResult.success_result(f"{len(rows)} file(s) import {rel}:\n" + "\n".join(f"  {r}" for r in rows[:LIMIT * 2]) if rows else f"No file imports {rel} (an entry point, a route file the framework loads by name, or dead code).")
        if act == "tests":
            rows = ig.tests_for(rel)
            return ToolResult.success_result(f"tests that cover {rel}:\n" + "\n".join(f"  {r}" for r in rows) if rows else f"No test file imports or is named after {rel}.")
        radius = ig.blast_radius(rel)  # act is blast, or impact without a graph snapshot
        if not radius:
            return ToolResult.success_result(f"Nothing imports {rel}, directly or through other files. Framework entry files (page.tsx, route.ts, layout.tsx) are loaded by name, not imported.")
        body = "\n".join(f"  {'·' * h} {f}" for f, h in radius)
        return ToolResult.success_result(f"depends on {rel} (indented by import hops: · direct, ·· via one file, ...):\n{body}\nRead the call sites of the direct ones before changing behaviour.")

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = GraphParams(**invocation.params)
        act = p.action.lower()
        if act in {"importers", "imports", "blast", "tests"} or (act == "impact" and codegraph.load(invocation.cwd) is None):
            return self._live(act, p.name, invocation.cwd)
        g = codegraph.load(invocation.cwd)
        if g is None:
            return ToolResult.error_result("No code graph found (graphify-out/graph.json). Use grep and read_file instead.")
        stale = g.staleness()
        note = f"\n\n({stale}; verify with read_file)" if stale else "\n\n(from the code graph snapshot; verify with read_file)"
        act = p.action.lower()

        if act == "impact":
            if not p.name:
                return ToolResult.error_result("`name` is required for this action")
            node, _ = g.resolve(p.name)
            target = g.rel(g.nodes[node].get("source_file", "")) if node else p.name
            rows = g.impact([target])
            if not rows:
                return ToolResult.success_result(f"No other file depends on {target} in the graph." + note)
            body = "\n".join(f"  {f}  ({', '.join(labels)})" for f, labels in rows)
            return ToolResult.success_result(f"files that depend on {target} (read their call sites before changing its behaviour):\n{body}" + note)

        if act == "stats":
            return ToolResult.success_result(f"{len(g.nodes)} nodes, {sum(len(v) for v in g.out.values())} edges." + note)
        if not p.name:
            return ToolResult.error_result("`name` is required for this action")

        if act == "find":
            hits = g.find(p.name, LIMIT)
            if not hits:
                return ToolResult.success_result(f"No graph node matches '{p.name}'. Try grep.")
            rows = [f"{g.nodes[h]['label']:<40} {kind_of(g.nodes[h]):<8} {g.where(h)}  [module {g.nodes[h].get('community')}, {g.degree(h)} links]" for h in hits]
            return ToolResult.success_result("\n".join(rows) + note)

        node, others = g.resolve(p.name)
        if node is None:
            return ToolResult.success_result(f"No graph node named '{p.name}'. Try action=find with part of the name, or grep.")
        head = f"{g.nodes[node]['label']} at {g.where(node)}" + (f"  (also matches: {', '.join(g.nodes[o]['label'] for o in others)})" if others else "")

        def fmt(pairs, arrow):
            rows, seen = [], set()
            for other, e in pairs:
                key = (other, e.get("relation"))
                if key in seen:
                    continue
                seen.add(key)
                mark = "~" if e.get("confidence") == "INFERRED" else " "
                rows.append(f"{mark} {e.get('relation', '?'):<10} {arrow} {g.nodes[other]['label']:<38} {g.where(other)}")
            return rows

        if act in {"callers", "callees"}:
            # the graph's edge orientation is unreliable, so direction is decided from the source text
            linked = {}
            for other, e in g.out[node] + g.inc[node]:
                if e.get("relation") == "calls" and other != node:
                    linked.setdefault(other, e)
            want = "caller" if act == "callers" else "callee"
            rows, unclear = [], []
            for other, e in linked.items():
                d = g.call_direction(node, other)
                mark = "~" if e.get("confidence") == "INFERRED" else " "
                line = f"{mark} {g.nodes[other]['label']:<38} {g.where(other)}"
                (rows if d == want else unclear if d == "unclear" else []).append(line)
            what = "call it" if act == "callers" else "it calls"
            out = f"{head}\n{len(rows)} that {what} (direction confirmed in the source text):\n" + "\n".join(rows[:LIMIT])
            if unclear:
                out += f"\n\ncall-linked but direction unclear ({len(unclear)}); confirm with grep:\n" + "\n".join(unclear[:10])
            return ToolResult.success_result(out + note)
        if act == "neighbors":
            rows = fmt(g.out[node], "->") + fmt(g.inc[node], "<-")
            return ToolResult.success_result(f"{head}\n" + "\n".join(rows[: LIMIT * 2]) + note)
        if act == "community":
            members = g.community(g.nodes[node].get("community"))
            files = sorted({g.rel(g.nodes[m].get("source_file", "")) for m in members})[:12]
            rows = [f"{g.nodes[m]['label']:<36} {g.where(m)}" for m in members[:LIMIT]]
            return ToolResult.success_result(f"{head}\nmodule {g.nodes[node].get('community')} has {len(members)} nodes; main files: {', '.join(files)}\ntop members:\n" + "\n".join(rows) + note)
        if act == "path":
            if not p.to:
                return ToolResult.error_result("`to` is required for action=path")
            other, _ = g.resolve(p.to)
            if other is None:
                return ToolResult.success_result(f"No graph node named '{p.to}'.")
            chain = g.path(node, other)
            if not chain:
                return ToolResult.success_result(f"No connection found between {p.name} and {p.to} in the graph.")
            return ToolResult.success_result(" -> ".join(f"{g.nodes[c]['label']} ({g.where(c)})" for c in chain) + note)
        return ToolResult.error_result(f"Unknown action '{p.action}'")
