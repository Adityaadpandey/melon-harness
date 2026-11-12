import { REPO } from "./docs";

// The newest GitHub release, cached for five minutes so we stay well under GitHub's rate limit.
// Returns null if GitHub is unreachable, so the page still renders.
export async function latestRelease(): Promise<{ version: string; url: string } | null> {
  try {
    const r = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
      headers: { accept: "application/vnd.github+json" },
      next: { revalidate: 300 },
    });
    if (!r.ok) return null;
    const j = await r.json();
    return { version: String(j.tag_name).replace(/^v/, ""), url: j.html_url };
  } catch {
    return null;
  }
}
