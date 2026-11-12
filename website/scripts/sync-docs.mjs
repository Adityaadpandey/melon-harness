// Copies what the site shows out of the repo root, so the README stays the one place the docs are written.
//   README.md   -> content/docs/<slug>.mdx + meta.json   (one page per "## " heading, read by Fumadocs)
//   scripts/    -> content/install.*                      (served at /install.sh and /install.ps1)
//   docs/assets -> public/docs/assets                     (images the docs point at)
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const site = join(dirname(fileURLToPath(import.meta.url)), "..");
const root = join(site, "..");
const out = join(site, "content/docs");
const REPO_BLOB = "https://github.com/Adityaadpandey/zerd.si/blob/main/";

// Sidebar groups, in reading order. A page missing from here lands in "More" so new docs never vanish.
const GROUPS = [
  ["Start here", ["try-it-in-two-minutes", "what-makes-it-different", "zerd-vs-claude-code-on-11-tasks", "zerd-in-numbers"]],
  ["How it works", ["the-idea-in-one-picture", "architecture", "life-of-a-request", "design-decisions", "context-how-zerd-stays-cheap", "finding-code-without-reading-everything", "correctness-verify-review-ground", "reviews-and-fixing-what-they-find", "adversarial-round-a-webhook-service"]],
  ["Using zerd", ["models-and-routing", "safety", "memory-and-learning", "parallel-agents", "skills-subagents-profiles", "interactive-ui", "extending-mcp-hooks-commands-project-tools", "configuration"]],
  ["Reference", ["tests", "windows-notes", "not-there-yet", "license"]],
];

// One sentence per page, written from what the page itself says.
const DESCRIPTIONS = {
  "try-it-in-two-minutes": "Install zerd on Mac, Linux or Windows with one command, or build it from source.",
  "what-makes-it-different": "What zerd does differently from most coding agents, from reviewing the real diff to checking every file path it cites.",
  "zerd-vs-claude-code-on-11-tasks": "The same 11 coding tasks and hidden tests run through zerd and Claude Code, with pass counts, tokens, cache share and wall time.",
  "zerd-in-numbers": "Size, test count, built-in surface and before-and-after measurements, taken from this repo.",
  "the-idea-in-one-picture": "zerd moves the hard parts of an agent run into code the model cannot skip, shown problem by problem.",
  architecture: "The layers of zerd and the one rule between them: everything the model can do goes through one door, and every decision is traced.",
  "life-of-a-request": "Follow one request from classification through the agent loop, verification, review and the citation check.",
  "design-decisions": "The choices that shaped zerd, each with its reason and trade-off.",
  "context-how-zerd-stays-cheap": "How zerd keeps the prompt small and stable by capping, archiving, pruning and folding old context.",
  "finding-code-without-reading-everything": "The tools zerd reaches for to locate code, cheapest first, from ranked search to symbol reads.",
  "correctness-verify-review-ground": "The three stages a run must clear to count as verified: the checks gate, an independent review and grounding.",
  "reviews-and-fixing-what-they-find": "How zerd reviews a working tree, commits, a branch or a PR, and how it fixes what the review finds.",
  "adversarial-round-a-webhook-service": "A harder second test: build a payment-webhook service, then attack it with 15 scenarios, for zerd and Claude Code alike.",
  "models-and-routing": "How zerd talks to providers and picks which model handles a request.",
  safety: "The independent layers that keep zerd inside your project folder and away from what it should not touch.",
  "memory-and-learning": "What zerd remembers per repo, where it is stored, and how lessons and edit history feed back into later runs.",
  "parallel-agents": "Read-only sub-agents, background and follow-up agents, and parallel tasks.",
  "skills-subagents-profiles": "Skills as short playbooks that attach automatically, plus sub-agents and profiles.",
  "interactive-ui": "The full-screen terminal UI: conversation, side panel, status line, approvals and ask_user.",
  "extending-mcp-hooks-commands-project-tools": "Automation flags, MCP servers, hooks, custom commands and project tools.",
  configuration: "Where config, keys and data live, and which layer wins when they disagree.",
  tests: "How the offline test suite is organised and where traces are written.",
  "windows-notes": "How zerd runs on Windows 10/11 and what differs from Mac and Linux.",
  "not-there-yet": "What zerd does not do yet, said plainly.",
  license: "zerd is source-available: run it for any purpose, but the source cannot be copied or redistributed.",
};

// same slug rule GitHub uses for heading anchors, so the README's own #links keep working
const slugify = (t) => t.toLowerCase().replace(/[^\p{L}\p{N}\s-]/gu, "").trim().replace(/\s+/g, "-");

const readme = readFileSync(join(root, "README.md"), "utf8");
const pages = readme
  .split(/^## /m)
  .slice(1) // drop the intro, the landing page covers it
  .map((p) => {
    const nl = p.indexOf("\n");
    const title = p.slice(0, nl).trim();
    return { slug: slugify(title), title, body: p.slice(nl + 1).trim() };
  })
  .filter((p) => p.title !== "Contents"); // the sidebar is the table of contents
const known = new Set(pages.map((p) => p.slug));

// Prose outside code is MDX, so stray < { } would break the build. Example: "<model>" must show as text.
function cleanProse(text) {
  return text
    // the README's centered <p><img></p> block becomes a plain markdown image
    .replace(/<p[^>]*>\s*<img\s+[^>]*?src="([^"]+)"[^>]*?alt="([^"]*)"[^>]*>\s*<\/p>/g, (_, src, alt) => `![${alt}](${src})`)
    .split(/(`[^`\n]*`)/) // leave inline code alone
    .map((chunk, i) => {
      if (i % 2) return chunk;
      return chunk
        .replace(/<br\s*\/?>/gi, "<br />") // valid JSX, keeps table cell line breaks
        .replace(/<(?!br \/>)/g, "&lt;")
        .replace(/\{/g, "&#123;")
        .replace(/\}/g, "&#125;");
    })
    .join("");
}

function rewriteLinks(text) {
  return text.replace(/\]\(([^)\s]+)\)/g, (m, href) => {
    if (/^(https?:|mailto:|\/)/.test(href)) return m;
    if (href.startsWith("#")) return known.has(href.slice(1)) ? `](/docs/${href.slice(1)})` : m;
    if (href.startsWith("docs/assets/")) return `](/${href})`;
    return `](${REPO_BLOB}${href.replace(/^\.\//, "")})`; // a repo file that has no page on the site
  });
}

function toMdx(body) {
  // split into code fences and prose, so only prose gets sanitised
  return body
    .split(/(^[ \t]*```[^\n]*\n[\s\S]*?^[ \t]*```[ \t]*$)/m)
    .map((seg, i) => {
      if (i % 2 === 0) return rewriteLinks(cleanProse(seg));
      const lang = /^\s*```(\S*)/.exec(seg)[1];
      if (lang !== "mermaid") return seg;
      const chart = seg.replace(/^\s*```[^\n]*\n/, "").replace(/\n\s*```[ \t]*$/, "");
      return `<Mermaid chart={${JSON.stringify(chart)}} />`;
    })
    .join("");
}

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
for (const p of pages) {
  const desc = DESCRIPTIONS[p.slug];
  const front = ["---", `title: ${JSON.stringify(p.title)}`, ...(desc ? [`description: ${JSON.stringify(desc)}`] : []), "---", ""].join("\n");
  writeFileSync(join(out, `${p.slug}.mdx`), front + toMdx(p.body) + "\n");
}

const listed = new Set(GROUPS.flatMap(([, s]) => s));
const rest = pages.map((p) => p.slug).filter((s) => !listed.has(s));
const sidebar = [...GROUPS, ...(rest.length ? [["More", rest]] : [])]
  .flatMap(([title, slugs]) => [`---${title}---`, ...slugs.filter((s) => known.has(s))]);
writeFileSync(join(out, "meta.json"), JSON.stringify({ pages: sidebar }, null, 2));

for (const f of ["install.sh", "install.ps1"]) cpSync(join(root, "scripts", f), join(site, "content", f));
mkdirSync(join(site, "public/docs"), { recursive: true });
cpSync(join(root, "docs/assets"), join(site, "public/docs/assets"), { recursive: true });
console.log(`synced ${pages.length} doc pages`);
