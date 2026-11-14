import { source } from "@/lib/source";
import { pageMarkdown } from "@/lib/markdown";

export const dynamic = "force-static";

// Every docs page as plain markdown in one response, so an AI agent can read the whole site in a single fetch.
export function GET() {
  const parts = source.getPages().map((p) => pageMarkdown(p.path, p.data.title, p.url));
  return new Response(parts.join("\n\n---\n\n"), { headers: { "content-type": "text/plain; charset=utf-8" } });
}
