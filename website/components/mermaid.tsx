"use client";
import { useEffect, useId, useState } from "react";
import { useTheme } from "next-themes";

// Colours match the docs palette, so a diagram reads as part of the page in either theme.
const THEMES = {
  light: { background: "#f1f2ee", primaryColor: "#e7e9e3", primaryTextColor: "#14171c", primaryBorderColor: "#2241e0", lineColor: "#4a505c", secondaryColor: "#dfe5ff", tertiaryColor: "#f1f2ee", clusterBkg: "#ebece7", clusterBorder: "#d4d7ce", edgeLabelBackground: "#f1f2ee" },
  dark: { background: "#0f1217", primaryColor: "#161a21", primaryTextColor: "#eceef2", primaryBorderColor: "#8da2ff", lineColor: "#a3a9b6", secondaryColor: "#1c2440", tertiaryColor: "#0f1217", clusterBkg: "#12161c", clusterBorder: "#252a34", edgeLabelBackground: "#0f1217" },
};

export function Mermaid({ chart }: { chart: string }) {
  const id = useId().replace(/:/g, "");
  const { resolvedTheme } = useTheme();
  const [svg, setSvg] = useState("");
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const { default: mermaid } = await import("mermaid");
        mermaid.initialize({
          startOnLoad: false,
          securityLevel: "strict",
          theme: "base",
          fontFamily: "var(--font-sans), system-ui, sans-serif",
          themeVariables: THEMES[resolvedTheme === "dark" ? "dark" : "light"],
        });
        const out = await mermaid.render(`m${id}`, chart);
        if (live) { setSvg(out.svg); setFailed(false); }
      } catch {
        if (live) setFailed(true); // bad diagram syntax: show the source instead of nothing
      }
    })();
    return () => { live = false; };
  }, [chart, id, resolvedTheme]);

  if (failed) return <pre className="mermaid-fallback"><code>{chart}</code></pre>;
  if (!svg) return <div className="mermaid-box" aria-busy="true" style={{ minHeight: 160 }} />;
  return <div className="mermaid-box" role="img" dangerouslySetInnerHTML={{ __html: svg }} />;
}
