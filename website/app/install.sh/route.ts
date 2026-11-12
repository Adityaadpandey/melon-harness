import { readFileSync } from "node:fs";
import { join } from "node:path";

// Served as plain text so `curl ... | sh` and `irm ... | iex` get the script itself, not an HTML page.
export const dynamic = "force-static";

export function GET() {
  const body = readFileSync(join(process.cwd(), "content/install.sh"), "utf8");
  return new Response(body, { headers: { "content-type": "text/plain; charset=utf-8" } });
}
