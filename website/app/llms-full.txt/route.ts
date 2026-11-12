import { readFileSync } from "node:fs";
import { join } from "node:path";
import { source } from "@/lib/source";
import { SITE } from "@/lib/site";

export const dynamic = "force-static";

// Every docs page as plain markdown in one response, so an AI agent can read the whole site in a single fetch.
export function GET() {
  const dir = join(process.cwd(), "content/docs");
  const parts = source.getPages().map((p) => {
    const raw = readFileSync(join(dir, p.path), "utf8").replace(/^---[\s\S]*?---\n/, "");
    return `# ${p.data.title}\nURL: ${SITE}${p.url}\n\n${raw.trim()}`;
  });
  return new Response(parts.join("\n\n---\n\n"), { headers: { "content-type": "text/plain; charset=utf-8" } });
}
