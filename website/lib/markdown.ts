import { readFileSync } from "node:fs";
import { join } from "node:path";
import { SITE } from "./site";

// A docs page as clean markdown for agents: no frontmatter, no JSX wrappers.
//   <Steps>, <Step>, <Callout ...>  -> dropped, their text stays
//   <Mermaid chart={`...`} />       -> a ```mermaid block, which agents can read as plain text
export function pageMarkdown(file: string, title: string, url: string): string {
  const raw = readFileSync(join(process.cwd(), "content/docs", file), "utf8").replace(/^---[\s\S]*?---\n/, "");
  const body = raw
    .replace(/<Mermaid\s+chart=\{`([\s\S]*?)`\}\s*\/>/g, (_, chart) => "```mermaid\n" + chart.trim() + "\n```")
    .replace(/^\s*<\/?(Steps?|Callout)\b[^>]*>\s*$/gm, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return `# ${title}\n\nSource: ${SITE}${url}\n\n${body}\n`;
}
