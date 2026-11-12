import type { Metadata } from "next";
import Link from "next/link";
import { groups, blurb } from "@/lib/docs";

export const metadata: Metadata = { title: "Docs" };

export default function DocsHome() {
  return (
    <div className="dhome">
      <h1>Documentation</h1>
      <p className="lede">How zerd works, how to use it, and where it still falls short.</p>
      <div className="dhome-groups">
        {groups.map((g) => (
          <section key={g.title}>
            <h2>{g.title}</h2>
            <ul>
              {g.pages.map((p) => (
                <li key={p.slug}>
                  <Link href={`/docs/${p.slug}`}>{p.title}</Link>
                  <span>{blurb(p.body)}</span>
                </li>
              ))}
            </ul>
          </section>
        ))}
      </div>
    </div>
  );
}
