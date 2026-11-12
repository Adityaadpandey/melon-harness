"use client";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState } from "react";

type G = { title: string; pages: { slug: string; title: string }[] };

export default function Sidebar({ groups }: { groups: G[] }) {
  const path = usePathname();
  const [q, setQ] = useState("");
  const [open, setOpen] = useState(false);

  // close the drawer after navigating
  useEffect(() => setOpen(false), [path]);

  const needle = q.trim().toLowerCase();
  const shown = groups
    .map((g) => ({ ...g, pages: g.pages.filter((p) => p.title.toLowerCase().includes(needle)) }))
    .filter((g) => g.pages.length);
  const current = groups.flatMap((g) => g.pages).find((p) => path === `/docs/${p.slug}`);

  return (
    <aside className={`dside${open ? " open" : ""}`}>
      <button type="button" className="dtoggle" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span>{current?.title ?? "Docs"}</span>
        <span aria-hidden>{open ? "Close" : "Menu"}</span>
      </button>
      <div className="dpanel">
        <input type="search" placeholder="Filter pages" aria-label="Filter pages" value={q} onChange={(e) => setQ(e.target.value)} />
        <nav>
          {shown.map((g) => (
            <div className="dgroup" key={g.title}>
              <h3>{g.title}</h3>
              {g.pages.map((p) => (
                <Link key={p.slug} href={`/docs/${p.slug}`} aria-current={path === `/docs/${p.slug}` ? "page" : undefined}>
                  {p.title}
                </Link>
              ))}
            </div>
          ))}
          {!shown.length && <p className="empty">No pages match.</p>}
        </nav>
      </div>
    </aside>
  );
}
