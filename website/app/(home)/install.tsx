"use client";
import { useState } from "react";
import CopyCmd from "./copy-cmd";

const TABS = [
  ["macOS / Linux", "curl -fsSL https://zerd.si/install.sh | sh"],
  ["Windows", "irm https://zerd.si/install.ps1 | iex"],
] as const;

export default function Install({ big = false }: { big?: boolean }) {
  const [i, setI] = useState(0);
  return (
    <div className="inst">
      <div className="inst-tabs" role="tablist" aria-label="Operating system">
        {TABS.map(([name], n) => (
          <button key={name} type="button" role="tab" aria-selected={i === n} onClick={() => setI(n)}>{name}</button>
        ))}
      </div>
      <CopyCmd key={i} cmd={TABS[i][1]} big={big} />
    </div>
  );
}
