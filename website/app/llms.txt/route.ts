import { source } from "@/lib/source";
import { FAQ } from "@/lib/faq";
import { DESCRIPTION, NAME, SITE, TAGLINE } from "@/lib/site";

export const dynamic = "force-static";

// Short index for AI agents (llmstxt.org): what zerd is, the common questions, then every docs page as a .md link.
export function GET() {
  const pages = source.getPages().map((p) => `- [${p.data.title}](${SITE}${p.url}.md): ${p.data.description ?? ""}`);
  const faq = FAQ.map(([q, a]) => `**${q}** ${a}`).join("\n\n");
  const body = `# ${NAME}, ${TAGLINE}\n\n> ${DESCRIPTION}\n\nHome page: ${SITE}\nInstall (macOS/Linux): \`curl -fsSL ${SITE}/install.sh | sh\`\nInstall (Windows): \`irm ${SITE}/install.ps1 | iex\`\nFull docs in one file: ${SITE}/llms-full.txt\n\n## Common questions\n\n${faq}\n\n## Docs\n\nEach link is the plain-markdown version of the page.\n\n${pages.join("\n")}\n`;
  return new Response(body, { headers: { "content-type": "text/plain; charset=utf-8" } });
}
