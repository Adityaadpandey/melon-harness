"use client";
import { useState } from "react";

// The install line. Click anywhere on it to copy; the label flips to "copied" for two seconds.
export default function CopyCmd({ cmd, big = false }: { cmd: string; big?: boolean }) {
  const [done, setDone] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(cmd);
      setDone(true);
      setTimeout(() => setDone(false), 2000);
    } catch {
      // clipboard blocked; the text is still selectable
    }
  };
  return (
    <div className={big ? "cmd big" : "cmd"}>
      <code><span aria-hidden="true">$ </span>{cmd}</code>
      <button type="button" onClick={copy} aria-label="Copy install command">{done ? "copied" : "copy"}</button>
      <span className="sr-only" role="status">{done ? "Install command copied" : ""}</span>
    </div>
  );
}
