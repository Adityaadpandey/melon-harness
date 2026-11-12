"use client";
import { useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

export type Row = [bad: string, good: [string, string, string], how: string];

// Each row opens to show how zerd enforces it. One open at a time keeps the list calm.
export default function Ledger({ rows }: { rows: Row[] }) {
  const [open, setOpen] = useState<number | null>(0);
  return (
    <ul className="ledger-list">
      {rows.map(([bad, [a, b, c], how], i) => {
        const on = open === i;
        return (
          <li key={bad} className={on ? "row on" : "row"}>
            <button type="button" aria-expanded={on} aria-controls={`how-${i}`} onClick={() => setOpen(on ? null : i)}>
              <span className="bad"><span className="sr-only">Most agents </span>{bad}</span>
              <span className="good"><span className="sr-only">zerd </span>{a}<strong>{b}</strong>{c}</span>
              <span className="plus" aria-hidden="true" />
            </button>
            <AnimatePresence initial={false}>
              {on && (
                <motion.div id={`how-${i}`} className="how-wrap" initial={{ height: 0, opacity: 0 }} animate={{ height: "auto", opacity: 1 }} exit={{ height: 0, opacity: 0 }} transition={{ duration: 0.45, ease: [0.22, 1, 0.36, 1] }}>
                  <p className="how">{how}</p>
                </motion.div>
              )}
            </AnimatePresence>
          </li>
        );
      })}
    </ul>
  );
}
