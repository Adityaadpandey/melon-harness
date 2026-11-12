"use client";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState } from "react";

export default function Sidebar({ items }: { items: { slug: string; title: string }[] }) {
  const path = usePathname();
  const [q, setQ] = useState("");
  const [open, setOpen] = useState(false);

  // close the drawer after navigating
  useEffect(() => setOpen(false), [path]);

  const shown = items.filter((p) => p.title.toLowerCase().includes(q.trim().toLowerCase()));
  const current = items.find((p) => path === `/docs/${p.slug}`);

  return (
    <aside className={`dside${open ? " open" : ""}`}>
      <button type="button" className="dtoggle" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span>{current?.title ?? "Docs"}</span>
        <span aria-hidden>{open ? "×" : "☰"}</span>
      </button>
      <div className="dpanel">
        <input
          type="search"
          placeholder="Filter pages"
          aria-label="Filter pages"
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
        <nav>
          {shown.map((p) => (
            <Link
              key={p.slug}
              href={`/docs/${p.slug}`}
              aria-current={path === `/docs/${p.slug}` ? "page" : undefined}
            >
              {p.title}
            </Link>
          ))}
          {!shown.length && <p className="empty">No pages match.</p>}
        </nav>
      </div>
    </aside>
  );
}
