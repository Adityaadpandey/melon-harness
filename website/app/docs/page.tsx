import type { Metadata } from "next";
import Link from "next/link";
import { DocsBody, DocsDescription, DocsPage, DocsTitle } from "fumadocs-ui/layouts/docs/page";
import { source } from "@/lib/source";

export const metadata: Metadata = { title: "Docs" };

// Groups come from the sidebar tree (the separators in meta.json), so this index never drifts from it.
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
      <DocsTitle>Documentation</DocsTitle>
      <DocsDescription>How zerd works, how to use it, and where it still falls short.</DocsDescription>
      <DocsBody>
        {groups().map((g) => (
          <section key={g.title} className="docs-group">
            <h2>{g.title}</h2>
            <div className="docs-cards">
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
