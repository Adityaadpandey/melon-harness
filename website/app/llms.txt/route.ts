import { source } from "@/lib/source";
import { DESCRIPTION, NAME, SITE, TAGLINE } from "@/lib/site";

export const dynamic = "force-static";

// Short index for AI agents (llmstxt.org): what zerd is, then every docs page with its one-liner.
export function GET() {
  const pages = source.getPages().map((p) => `- [${p.data.title}](${SITE}${p.url}): ${p.data.description ?? ""}`);
  const body = `# ${NAME}, ${TAGLINE}\n\n> ${DESCRIPTION}\n\nInstall (macOS/Linux): \`curl -fsSL ${SITE}/install.sh | sh\`\nInstall (Windows): \`irm ${SITE}/install.ps1 | iex\`\nFull docs in one file: ${SITE}/llms-full.txt\n\n## Docs\n\n${pages.join("\n")}\n`;
  return new Response(body, { headers: { "content-type": "text/plain; charset=utf-8" } });
}
