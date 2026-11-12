"use client";
import Link from "next/link";
import { motion, useMotionValueEvent, useScroll } from "framer-motion";
import { useState } from "react";
import Logo from "./logo";

// Frosted glass bar. It floats a little away from the top edge and tightens once you scroll.
export default function Nav({ repo }: { repo: string }) {
  const { scrollY } = useScroll();
  const [tight, setTight] = useState(false);
  useMotionValueEvent(scrollY, "change", (v) => setTight(v > 24));
  return (
    <motion.header className={`sk-nav${tight ? " tight" : ""}`} initial={{ y: -24, opacity: 0 }} animate={{ y: 0, opacity: 1 }} transition={{ duration: 0.9, ease: [0.22, 1, 0.36, 1] }}>
      <div className="sk-nav-in">
        <Logo />
        <nav aria-label="Main">
          <Link href="/docs/try-it-in-two-minutes">Docs</Link>
          <Link href="/docs/models-and-routing">Models</Link>
          <a href={`https://github.com/${repo}/releases`}>Releases</a>
          <a className="sk-pill" href="#install">Install</a>
        </nav>
      </div>
    </motion.header>
  );
}
