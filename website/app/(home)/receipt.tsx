"use client";
import { motion, useMotionValue, useReducedMotion, useSpring, useTransform } from "framer-motion";
import { useEffect } from "react";
import { Cloud } from "./clouds";

// The thing zerd hands you instead of "Done!": a receipt for what it actually checked.
// It prints out of a cloud on load, sways like it is hanging, leans toward the pointer,
// and a stamp lands once it has finished printing. The lines come from the real recording in the
// window below it: the file it changed, the pytest result and what its own answer said it did not check.
const LINES: [mark: "ok" | "bad" | "skip" | "plain", text: string, note?: string][] = [
  ["plain", "checks on 1 changed file"],
  ["ok", "pytest test_client.py", "3 passed"],
  ["plain", "what changed"],
  ["ok", "retry_lab/client.py", "+6 −3"],
  ["plain", "said, but not checked"],
  ["skip", "the rest of the suite", "not run"],
];

export default function Receipt() {
  const still = useReducedMotion();
  // Pointer position over the whole window, -1 to 1 either side of the middle.
  const px = useMotionValue(0);
  const py = useMotionValue(0);
  const sx = useSpring(px, { stiffness: 60, damping: 16 });
  const sy = useSpring(py, { stiffness: 60, damping: 16 });
  const rotY = useTransform(sx, [-1, 1], [-9, 9]);
  const rotX = useTransform(sy, [-1, 1], [6, -6]);

  useEffect(() => {
    if (still) return;
    const move = (e: PointerEvent) => {
      px.set((e.clientX / innerWidth) * 2 - 1);
      py.set((e.clientY / innerHeight) * 2 - 1);
    };
    addEventListener("pointermove", move);
    return () => removeEventListener("pointermove", move);
  }, [still, px, py]);

  return (
    <div className="receipt-slot" aria-label="A zerd receipt: one file changed, three tests passed, and one thing it says it did not check" role="img">
      <motion.div className="receipt-tilt" style={still ? undefined : { rotateX: rotX, rotateY: rotY }}>
        <div className="receipt-sway">
          {/* the paper comes out from under this cloud */}
          <div className="receipt-mouth" aria-hidden="true"><Cloud shape="d" /></div>
          <div className="receipt-clip">
            <div className="receipt-feed">
              <div className="receipt-paper">
                <p className="r-brand">zerd</p>
                <p className="r-ask">fix send_with_retry backoff</p>
                <hr />
                {LINES.map(([k, a, b]) => (
                  <p key={a} className={`r-line r-${k}`}>
                    {k !== "plain" && <i>{k === "ok" ? "✓" : k === "bad" ? "✗" : "–"}</i>}
                    <span>{a}</span>
                    {b && <em>{b}</em>}
                  </p>
                ))}
                <hr />
                <p className="r-total"><span>checked</span> 1 file · 1 check ok</p>
                <p className="r-small">worked for 16s · 1.9k tokens</p>
                <div className="r-bars" />
              </div>
            </div>
          </div>
          <motion.div className="receipt-stamp" initial={still ? false : { opacity: 0, scale: 2.4, rotate: -22 }} animate={{ opacity: 1, scale: 1, rotate: -9 }} transition={{ delay: still ? 0 : 5.3, type: "spring", stiffness: 420, damping: 18 }}>
            checked
          </motion.div>
        </div>
      </motion.div>
    </div>
  );
}
