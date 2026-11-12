"use client";
import { useState } from "react";

// Install command with a copy button. Shows "Copied" for a moment after a click.
export default function CopyCmd({ cmd }: { cmd: string }) {
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
    <div className="cmdbox">
      <span className="cmdbox-prompt">$</span>
      <code>{cmd}</code>
      <button type="button" onClick={copy} aria-label="Copy install command">
        {done ? "Copied" : "Copy"}
      </button>
    </div>
  );
}
