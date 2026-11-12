import type { MetadataRoute } from "next";
import { SITE } from "@/lib/site";

export const dynamic = "force-static";

// Everything public is open, AI crawlers included: being quoted by answer engines is the point.
export default function robots(): MetadataRoute.Robots {
  return { rules: [{ userAgent: "*", allow: "/", disallow: "/api/" }], sitemap: `${SITE}/sitemap.xml`, host: SITE };
}
