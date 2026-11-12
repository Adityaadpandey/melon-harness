import GithubSlugger from "github-slugger";

export type Heading = { id: string; text: string; level: 2 | 3 };

// Pulls h2/h3 out of raw markdown. Fenced code is skipped so a "## comment"
// inside a shell snippet never shows up in the table of contents.
// Slugs come from the same slugger rehype-slug uses, so the ids match.
export function headingsOf(md: string): Heading[] {
  const slugger = new GithubSlugger();
  const out: Heading[] = [];
  let fence = "";
  for (const line of md.split("\n")) {
    const f = line.match(/^\s*(`{3,}|~{3,})/);
    if (f) {
      if (!fence) fence = f[1][0];
      else if (f[1][0] === fence) fence = "";
      continue;
    }
    if (fence) continue;
    const m = line.match(/^(#{1,6})\s+(.+?)\s*#*\s*$/);
    if (!m) continue;
    const text = m[2]
      .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
      .replace(/[`*_~]/g, "");
    const id = slugger.slug(text);
    const level = m[1].length;
    if (level === 2 || level === 3) out.push({ id, text, level });
  }
  return out;
}
