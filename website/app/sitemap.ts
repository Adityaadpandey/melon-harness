import type { MetadataRoute } from "next";
import { source } from "@/lib/source";
import { SITE } from "@/lib/site";

export const dynamic = "force-static";

export default function sitemap(): MetadataRoute.Sitemap {
  const now = new Date();
  return [
    { url: SITE, lastModified: now, changeFrequency: "weekly", priority: 1 },
    { url: `${SITE}/docs`, lastModified: now, changeFrequency: "weekly", priority: 0.8 },
    ...source.getPages().map((p) => ({ url: `${SITE}${p.url}`, lastModified: now, changeFrequency: "monthly" as const, priority: 0.6 })),
  ];
}
