"use client";
import { useState } from "react";

// The install command as a tear-off ticket: the command on the left, a stub on the right.
// Tearing the stub (clicking it) copies the command and stamps it.
export default function CopyCmd({ cmd, big = false }: { cmd: string; big?: boolean }) {
  const [done, setDone] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(cmd);
      setDone(true);
      setTimeout(() => setDone(false), 2200);
    } catch {
      // clipboard blocked; the text is still selectable
    }
  };
  return (
    <div className={big ? "cmd-wrap big" : "cmd-wrap"}>
      <div className={big ? "cmd big" : "cmd"}>
        <code><span aria-hidden="true">$ </span>{cmd}</code>
        <button type="button" onClick={copy} aria-label="Copy install command">
          {done ? "Copied" : "Copy"}
        </button>
        {done && <span className="copy-stamp" aria-hidden="true">copied</span>}
        <span className="sr-only" role="status">{done ? "Install command copied" : ""}</span>
      </div>
    </div>
  );
}
