import { ImageResponse } from "next/og";
import { notFound } from "next/navigation";
import { source } from "@/lib/source";
import { DOC_SEO } from "@/lib/seo";
import { NAME } from "@/lib/site";

export const dynamic = "force-static";

// A catch-all can't hold a file-based opengraph-image, so this is a plain route: /og/quickstart, /og/models, ...
export function generateStaticParams() {
  return source.generateParams();
}

// One social card per docs page, with that page's own title, so a shared link says what it is about.
export async function GET(_req: Request, { params }: { params: Promise<{ slug: string[] }> }) {
  const page = source.getPage((await params).slug);
  if (!page) notFound();
  const title = DOC_SEO[page.slugs.join("/")]?.title ?? page.data.title;
  return new ImageResponse(
    (
      <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "column", justifyContent: "space-between", padding: 80, background: "#0b1020", color: "#fff" }}>
        <div style={{ display: "flex", fontSize: 40, color: "#c3a6ff" }}>{NAME} docs</div>
        <div style={{ display: "flex", fontSize: title.length > 48 ? 64 : 76, fontWeight: 700, lineHeight: 1.12 }}>{title}</div>
        <div style={{ display: "flex", fontSize: 30, color: "#9fb0d0" }}>zerd.si · the terminal AI coding agent that verifies its own work</div>
      </div>
    ),
    { width: 1200, height: 630 },
  );
}
