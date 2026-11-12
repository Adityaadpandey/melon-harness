"use client";
import Link from "next/link";
import { motion, useReducedMotion, useScroll, useTransform } from "framer-motion";
import { CloudLayer, type Placed } from "./clouds";
import Demo from "./demo";
import Receipt from "./receipt";

const EASE = [0.22, 1, 0.36, 1] as const;

// Far clouds barely move, near ones travel further: that gap is what reads as depth.
const BACK: Placed[] = [
  { shape: "c", left: "-8%", top: 90, width: "46vw", speed: 0.05, drift: 220, tone: 0.8 },
  { shape: "b", left: "58%", top: 40, width: "42vw", speed: 0.07, drift: 260, delay: 90, tone: 0.85 },
  { shape: "d", left: "28%", top: 380, width: "38vw", speed: 0.1, drift: 190, delay: 40, tone: 0.7 },
];
const FRONT: Placed[] = [
  { shape: "a", left: "-14%", top: 1260, width: "58vw", speed: 0.2, drift: 150, delay: 30, z: 3 },
  { shape: "c", left: "52%", top: 1300, width: "64vw", speed: 0.26, drift: 170, delay: 100, z: 3 },
];

// A line that rises out from behind a mask, one after another.
function Line({ children, i, className = "" }: { children: React.ReactNode; i: number; className?: string }) {
  return (
    <span className="mask">
      <motion.span className={`block ${className}`} initial={{ y: "108%" }} animate={{ y: 0 }} transition={{ duration: 1.1, delay: 0.25 + i * 0.16, ease: EASE }}>
        {children}
      </motion.span>
    </span>
  );
}

export default function Hero() {
  const still = useReducedMotion();
  const { scrollY } = useScroll();
  const lift = useTransform(scrollY, [0, 800], [0, -70]);
  const fade = useTransform(scrollY, [0, 500], [1, 0.0]);
  return (
    <section className="hero">
      <CloudLayer clouds={BACK} />
      <div className="wrap hero-copy">
        <Receipt />
        <h1>
          <Line i={0} className="h-sans">Most agents say done.</Line>
          <Line i={1} className="h-serif">zerd shows the receipt.</Line>
        </h1>
        <motion.div className="hero-aside" initial={{ opacity: 0, y: 18 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 1, delay: 0.95, ease: EASE }} style={still ? undefined : { opacity: fade }}>
          <p>
            zerd is a terminal coding agent. After it edits your code it runs the checks that cover your changed files,
            has a second model review the real diff, and looks up every file path it mentions.
          </p>
          <div className="hero-cta">
            <a className="btn-dark" href="#install"><span>Install zerd</span><b aria-hidden="true">↓</b></a>
            <Link className="btn-glass" href="/docs/try-it-in-two-minutes">Read the docs</Link>
          </div>
          <p className="hero-fine">One native binary, about 11 MB, for macOS, Linux and Windows.</p>
        </motion.div>
      </div>

      <motion.div className="wrap hero-win" initial={{ opacity: 0, y: 90 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 1.3, delay: 1.1, ease: EASE }}>
        <motion.div style={still ? undefined : { y: lift }}>
          <Demo />
        </motion.div>
      </motion.div>
      <CloudLayer clouds={FRONT} className="front" />
    </section>
  );
}
