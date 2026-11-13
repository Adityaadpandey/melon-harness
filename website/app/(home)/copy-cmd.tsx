"use client";
import { useState } from "react";

// The install command with a copy button. Selecting the text by hand works too.
export default function CopyCmd({ cmd }: { cmd: string }) {
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
    <div className="cmd">
      <code><span aria-hidden="true">$ </span>{cmd}</code>
      <button type="button" onClick={copy}>{done ? "Copied" : "Copy"}</button>
      <span className="sr-only" role="status">{done ? "Install command copied" : ""}</span>
    </div>
  );
}
