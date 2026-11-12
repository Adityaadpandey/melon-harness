"use client";
import { useState } from "react";

// Install command with a copy button. Says "Copied" for a moment after a click.
export default function CopyCmd({ cmd, big = false }: { cmd: string; big?: boolean }) {
  const [done, setDone] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(cmd);
      setDone(true);
      setTimeout(() => setDone(false), 1600);
    } catch {
      // clipboard blocked; the text is still selectable
    }
  };
  return (
    <div className={big ? "cmd big" : "cmd"}>
      <code><span aria-hidden="true">$ </span>{cmd}</code>
      <button type="button" onClick={copy} aria-label="Copy install command">
        {done ? "Copied" : "Copy"}
      </button>
      <span className="sr-only" role="status">{done ? "Install command copied" : ""}</span>
    </div>
  );
}
