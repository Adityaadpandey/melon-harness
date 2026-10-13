from pydantic import BaseModel, Field

from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from utils import codegraph
from utils.codegraph import kind_of

LIMIT = 25


class GraphParams(BaseModel):
    action: str = Field(
        ...,
        description="find (locate symbols/files by name) | callers (who calls NAME; direction checked in source) | callees (what NAME calls) | "
        "neighbors (all relations of NAME) | path (shortest connection NAME -> TO) | community (the module NAME belongs to) | stats",
    )
    name: str | None = Field(None, description="Symbol, class or file name (e.g. delTicketTypeCache, ticket.service.ts)")
    to: str | None = Field(None, description="Second name, for action=path")


class GraphTool(Tool):
    name = "graph"
    description = (
        "Query the prebuilt code graph: where something is defined, who calls it, what it calls, how two things connect. "
        "One call replaces several grep/read rounds for structure questions. It is a snapshot: confirm with read_file before relying on a line."
    )
    kind = ToolKind.READ
    schema = GraphParams

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        p = GraphParams(**invocation.params)
        g = codegraph.load(invocation.cwd)
        if g is None:
            return ToolResult.error_result("No code graph found (graphify-out/graph.json). Use grep and read_file instead.")
        stale = g.staleness()
        note = f"\n\n({stale}; verify with read_file)" if stale else "\n\n(from the code graph snapshot; verify with read_file)"
        act = p.action.lower()

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
