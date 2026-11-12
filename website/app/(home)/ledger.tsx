"use client";
import { useState } from "react";

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
              <span className="bad"><span className="sr-only">Most agents </span><i className="m m-bad" aria-hidden="true">✗</i>{bad}</span>
              <span className="good"><span className="sr-only">zerd </span><i className="m m-good" aria-hidden="true">✓</i>{a}<strong>{b}</strong>{c}</span>
              <span className="plus" aria-hidden="true" />
            </button>
            {on && <div id={`how-${i}`}><p className="how">{how}</p></div>}
          </li>
        );
      })}
    </ul>
  );
}
