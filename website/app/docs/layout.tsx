import Link from "next/link";
import { pages } from "@/lib/docs";

export default function DocsLayout({ children }: { children: React.ReactNode }) {
  return (
    <div className="docs">
      <aside>
        {pages.map((p) => (
          <Link key={p.slug} href={`/docs/${p.slug}`}>{p.title}</Link>
        ))}
      </aside>
      <article>{children}</article>
    </div>
  );
}
