import type { Metadata } from "next";
import Link from "next/link";
import { DocsBody, DocsDescription, DocsPage, DocsTitle } from "fumadocs-ui/layouts/docs/page";
import { source } from "@/lib/source";
import { pageMeta } from "@/lib/seo";

export const metadata: Metadata = pageMeta({
  path: "/docs",
  image: "/opengraph-image",
  title: "zerd docs: install, models, safety, CLI reference",
  description: "Learn zerd, the terminal AI coding agent: install in two minutes, see how every run is verified, and read the full CLI, config and tools reference.",
  keywords: ["zerd docs", "AI coding agent documentation", "coding agent CLI reference"],
});

// Groups come from the sidebar tree (the separators in meta.json), so this index never drifts from it.
// One line per sidebar group, saying who it is for. Keyed by the separator names in meta.json.
const BLURBS: Record<string, string> = {
  "Start here": "New here? Read these three in order.",
  Guides: "Everyday use: what to type, which model to pick, and how zerd keeps your code safe.",
  "Under the hood": "How zerd decides a change is actually finished.",
  Reference: "Every flag, setting and command, for when you know what you’re after.",
  Evidence: "Head-to-head results, honest limits, and the license.",
};

function groups() {
  const out: { title: string; pages: { url: string; name: string; description?: string }[] }[] = [];
  for (const node of source.getPageTree().children) {
    if (node.type === "separator") out.push({ title: String(node.name), pages: [] });
    else if (node.type === "page" && out.length)
      out.at(-1)!.pages.push({
        url: node.url,
        name: String(node.name),
        description: source.getPages().find((p) => p.url === node.url)?.data.description,
      });
  }
  return out;
}

export default function DocsIndex() {
  return (
    <DocsPage toc={[]}>
      <DocsTitle>Learn zerd</DocsTitle>
      <DocsDescription>zerd is a terminal coding agent that checks its own work. Install it in two minutes, see how a run is verified, and read where it still falls short.</DocsDescription>
      <DocsBody>
        {groups().map((g, i) => (
          <section key={g.title} className="docs-group">
            <h2>{g.title}</h2>
            {BLURBS[g.title] && <p className="docs-blurb">{BLURBS[g.title]}</p>}
            <div className={`docs-cards${i === 0 ? " docs-featured" : ""}`}>
              {g.pages.map((p) => (
                <Link key={p.url} href={p.url} className="docs-card">
                  <strong>{p.name}</strong>
                  {p.description && <span>{p.description}</span>}
                </Link>
              ))}
            </div>
          </section>
        ))}
      </DocsBody>
    </DocsPage>
  );
}
