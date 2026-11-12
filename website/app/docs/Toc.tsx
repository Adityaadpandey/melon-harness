"use client";
import { useEffect, useState } from "react";
import type { Heading } from "@/lib/headings";

export default function Toc({ headings }: { headings: Heading[] }) {
  const [active, setActive] = useState("");

  useEffect(() => {
    const els = headings.map((h) => document.getElementById(h.id)).filter(Boolean) as HTMLElement[];
    // the last heading that has scrolled past the top band wins
    const onScroll = () => {
      let cur = els[0]?.id ?? "";
      for (const el of els) if (el.getBoundingClientRect().top <= 100) cur = el.id;
      setActive(cur);
    };
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, [headings]);

  if (!headings.length) return null;
  return (
    <aside className="dtoc">
      <h4>On this page</h4>
      {headings.map((h) => (
        <a key={h.id} href={`#${h.id}`} className={`l${h.level}${active === h.id ? " on" : ""}`}>
          {h.text}
        </a>
      ))}
    </aside>
  );
}
