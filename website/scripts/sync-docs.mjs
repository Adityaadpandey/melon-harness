// Copies what the site shows out of the repo root, so the README stays the one place the docs are written.
//   README.md  -> content/docs.json   (one page per "## " heading)
//   scripts/   -> content/install.*   (served at /install.sh and /install.ps1)
//   docs/assets -> public/docs/assets (images the docs point at)
import { cpSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const site = join(dirname(fileURLToPath(import.meta.url)), "..");
const root = join(site, "..");
mkdirSync(join(site, "content"), { recursive: true });

// same slug rule GitHub uses for heading anchors, so the README's own #links keep working
const slugify = (t) => t.toLowerCase().replace(/[^\p{L}\p{N}\s-]/gu, "").trim().replace(/\s+/g, "-");

const readme = readFileSync(join(root, "README.md"), "utf8");
const parts = readme.split(/^## /m).slice(1); // drop the intro, the landing page covers it
const pages = parts
  .map((p) => {
    const nl = p.indexOf("\n");
    const title = p.slice(0, nl).trim();
    return { slug: slugify(title), title, body: p.slice(nl + 1).trim() };
  })
  .filter((p) => p.title !== "Contents"); // the sidebar is the table of contents

const known = new Set(pages.map((p) => p.slug));
for (const p of pages) {
  p.body = p.body
    .replaceAll("](docs/assets/", "](/docs/assets/")
    .replace(/\]\(#([^)]+)\)/g, (m, id) => (known.has(id) ? `](/docs/${id})` : m));
}
writeFileSync(join(site, "content/docs.json"), JSON.stringify(pages));

for (const f of ["install.sh", "install.ps1"]) cpSync(join(root, "scripts", f), join(site, "content", f));
mkdirSync(join(site, "public/docs"), { recursive: true });
cpSync(join(root, "docs/assets"), join(site, "public/docs/assets"), { recursive: true });
console.log(`synced ${pages.length} doc pages`);
