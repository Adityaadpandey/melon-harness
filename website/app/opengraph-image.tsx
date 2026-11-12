import { ImageResponse } from "next/og";
import { NAME, TAGLINE } from "@/lib/site";

export const dynamic = "force-static";
export const alt = `${NAME}, ${TAGLINE}`;
export const size = { width: 1200, height: 630 };
export const contentType = "image/png";

// Social card shared by every page that doesn't set its own.
export default function Image() {
  return new ImageResponse(
    (
      <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "column", justifyContent: "center", padding: 90, background: "#0b1020", color: "#fff" }}>
        <div style={{ fontSize: 120, fontWeight: 700 }}>{NAME}</div>
        <div style={{ fontSize: 56, marginTop: 20, color: "#c3a6ff" }}>{TAGLINE}</div>
        <div style={{ fontSize: 30, marginTop: 40, color: "#9fb0d0" }}>Checks your changes · reviews the real diff · verifies every path</div>
      </div>
    ),
    size,
  );
}
