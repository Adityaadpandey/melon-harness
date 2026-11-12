"use client";
import { motion, useReducedMotion, useScroll, useTransform, type MotionValue } from "framer-motion";
import { useRef } from "react";

// A cloud is a handful of soft blobs, blurred, then pushed around by fractal noise so the edges
// look billowy instead of round. A darker copy sits slightly lower to give the belly some shade.
// Everything is drawn once as SVG; moving it later is only a transform, which is cheap.
type Blob = [cx: number, cy: number, rx: number, ry: number];
const SHAPES: Record<string, { seed: number; blobs: Blob[] }> = {
  a: { seed: 4, blobs: [[180, 210, 150, 62], [330, 170, 150, 92], [500, 150, 150, 108], [650, 195, 150, 74], [420, 230, 330, 52]] },
  b: { seed: 11, blobs: [[140, 220, 120, 52], [280, 180, 140, 82], [440, 205, 150, 70], [590, 215, 130, 50], [360, 240, 280, 44]] },
  c: { seed: 23, blobs: [[200, 200, 170, 78], [380, 150, 140, 100], [560, 170, 160, 90], [700, 220, 110, 48], [450, 240, 340, 48]] },
  d: { seed: 37, blobs: [[160, 230, 130, 44], [320, 200, 170, 68], [520, 190, 160, 74], [660, 225, 120, 46], [400, 250, 300, 38]] },
};

export function Cloud({ shape = "a", tone = 1 }: { shape?: keyof typeof SHAPES; tone?: number }) {
  const { seed, blobs } = SHAPES[shape];
  const id = `cloud-${shape}`;
  return (
    <svg viewBox="0 0 800 340" aria-hidden="true" focusable="false" style={{ width: "100%", height: "auto", overflow: "visible", display: "block" }}>
      <defs>
        <filter id={`${id}-w`} x="-25%" y="-40%" width="150%" height="200%">
          <feGaussianBlur in="SourceGraphic" stdDeviation="16" result="b" />
          <feTurbulence type="fractalNoise" baseFrequency="0.011 0.017" numOctaves="5" seed={seed} result="n" />
          <feDisplacementMap in="b" in2="n" scale="105" xChannelSelector="R" yChannelSelector="G" result="d" />
          <feGaussianBlur in="d" stdDeviation="2.5" />
        </filter>
        <filter id={`${id}-s`} x="-25%" y="-40%" width="150%" height="200%">
          <feGaussianBlur in="SourceGraphic" stdDeviation="24" result="b" />
          <feTurbulence type="fractalNoise" baseFrequency="0.011 0.017" numOctaves="5" seed={seed} result="n" />
          <feDisplacementMap in="b" in2="n" scale="105" xChannelSelector="R" yChannelSelector="G" />
        </filter>
      </defs>
      <g filter={`url(#${id}-s)`} fill="#8fb3dc" opacity={0.5 * tone} transform="translate(0 30)">
        {blobs.map((b, i) => <ellipse key={i} cx={b[0]} cy={b[1]} rx={b[2]} ry={b[3]} />)}
      </g>
      <g filter={`url(#${id}-w)`} fill="#fff" opacity={Math.min(1, 0.96 * tone)}>
        {blobs.map((b, i) => <ellipse key={i} cx={b[0]} cy={b[1] - 8} rx={b[2]} ry={b[3]} />)}
      </g>
    </svg>
  );
}

// One placed cloud. `speed` is how far it travels as the page scrolls (bigger = closer to you),
// `drift` is how many seconds one slow sway takes.
export type Placed = {
  shape: keyof typeof SHAPES;
  left: string; top: number; width: string;
  speed: number; drift: number; delay?: number; tone?: number; z?: number;
};

function Parallax({ c, progress }: { c: Placed; progress: MotionValue<number> }) {
  const y = useTransform(progress, [0, 1], [c.speed * 800, -c.speed * 800]);
  const still = useReducedMotion();
  return (
    <motion.div className="cloud" style={{ left: c.left, top: c.top, width: c.width, y: still ? 0 : y, zIndex: c.z ?? 0 }}>
      <div className="cloud-drift" style={{ animationDuration: `${c.drift}s`, animationDelay: `-${c.delay ?? 0}s` }}>
        <Cloud shape={c.shape} tone={c.tone} />
      </div>
    </motion.div>
  );
}

export function CloudLayer({ clouds, className = "" }: { clouds: Placed[]; className?: string }) {
  // Progress is measured against this layer's own section, so clouds stay put relative to it.
  const ref = useRef<HTMLDivElement>(null);
  const { scrollYProgress } = useScroll({ target: ref, offset: ["start end", "end start"] });
  return (
    <div className={`cloud-layer ${className}`} aria-hidden="true" ref={ref}>
      {clouds.map((c, i) => <Parallax c={c} progress={scrollYProgress} key={i} />)}
    </div>
  );
}
