import { readFileSync } from "node:fs";
import { join } from "node:path";

export type Page = { slug: string; title: string; body: string };

// content/docs.json is written by scripts/sync-docs.mjs before dev and build
export const pages: Page[] = JSON.parse(readFileSync(join(process.cwd(), "content/docs.json"), "utf8"));

export const REPO = "Adityaadpandey/zerd.si";

// Sidebar and docs home grouping. Order here is the reading order.
const GROUPS: { title: string; slugs: string[] }[] = [
  { title: "Start here", slugs: ["try-it-in-two-minutes", "what-makes-it-different", "zerd-vs-claude-code-on-11-tasks", "zerd-in-numbers"] },
  { title: "How it works", slugs: ["the-idea-in-one-picture", "architecture", "life-of-a-request", "design-decisions", "context-how-zerd-stays-cheap", "finding-code-without-reading-everything", "correctness-verify-review-ground", "reviews-and-fixing-what-they-find", "adversarial-round-a-webhook-service"] },
  { title: "Using zerd", slugs: ["models-and-routing", "safety", "memory-and-learning", "parallel-agents", "skills-subagents-profiles", "interactive-ui", "extending-mcp-hooks-commands-project-tools", "configuration"] },
  { title: "Reference", slugs: ["tests", "windows-notes", "not-there-yet", "license"] },
];

export type Group = { title: string; pages: Page[] };

// Any page missing from GROUPS lands in "Other" so new docs never vanish.
export const groups: Group[] = (() => {
  const listed = new Set(GROUPS.flatMap((g) => g.slugs));
  const out = GROUPS.map((g) => ({
    title: g.title,
    pages: g.slugs.map((s) => pages.find((p) => p.slug === s)).filter(Boolean) as Page[],
  }));
  const rest = pages.filter((p) => !listed.has(p.slug));
  if (rest.length) out.push({ title: "Other", pages: rest });
  return out.filter((g) => g.pages.length);
})();

// Pages in grouped order, used for prev/next.
export const ordered: Page[] = groups.flatMap((g) => g.pages);

export const groupOf = (slug: string) => groups.find((g) => g.pages.some((p) => p.slug === slug))?.title;

export const readingMinutes = (body: string) => Math.max(1, Math.round(body.split(/\s+/).length / 220));

// First plain paragraph of the body, markdown stripped, cut near 110 chars.
export function blurb(body: string, max = 110): string {
  let fence = false;
  for (const raw of body.split("\n")) {
    const line = raw.trim();
    if (/^(`{3,}|~{3,})/.test(line)) { fence = !fence; continue; }
    if (fence || !line || /^([#>|!\-*+\d]|<|\[!)/.test(line) && !/^\*\*|^\*[^*\s]/.test(line)) continue;
    const text = line
      .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
      .replace(/[`*_~]/g, "")
      .trim();
    // skip lead-ins like "Run this:" and fragments that start mid-sentence
    if (text.length < 30 || text.endsWith(":") || !/^[A-Z0-9]/.test(text)) continue;
    const first = text.match(/^.*?[.!?](?=\s|$)/)?.[0] ?? text;
    if (first.length <= max) return first;
    return first.slice(0, max).replace(/\s+\S*$/, "") + "…";
  }
  return "";
}
