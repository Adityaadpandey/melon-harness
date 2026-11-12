import { latestRelease } from "@/lib/latest";

// GET /api/latest -> { version, url }. `zerd update` and the installers can ask this instead of scraping GitHub.
export async function GET() {
  const r = await latestRelease();
  if (!r) return Response.json({ error: "release info unavailable" }, { status: 502 });
  return Response.json(r, { headers: { "cache-control": "public, s-maxage=300" } });
}
