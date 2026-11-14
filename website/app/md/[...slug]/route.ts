import { notFound } from "next/navigation";
import { source } from "@/lib/source";
import { pageMarkdown } from "@/lib/markdown";
import { SITE } from "@/lib/site";

export const dynamic = "force-static";

// Serves /docs/quickstart.md (rewritten to here in next.config.mjs) so an agent can read one page without the whole site.
export function generateStaticParams() {
  return source.generateParams();
}

export async function GET(_req: Request, { params }: { params: Promise<{ slug: string[] }> }) {
  const page = source.getPage((await params).slug);
  if (!page) notFound();
  return new Response(pageMarkdown(page.path, page.data.title, page.url), { headers: { "content-type": "text/markdown; charset=utf-8", link: `<${SITE}${page.url}>; rel="canonical"` } });
}
