import type { MetadataRoute } from "next";
import { source } from "@/lib/source";
import { SITE } from "@/lib/site";
import { lastModified } from "@/lib/lastmod";

export const dynamic = "force-static";

// Google ignores priority and changefreq, so they are left out. lastModified is the real git date of each
// page's source file, and left out when git can't tell us, rather than claiming everything changed today.
export default function sitemap(): MetadataRoute.Sitemap {
  const docs = source.getPages().map((p) => ({ url: `${SITE}${p.url}`, lastModified: lastModified(`content/docs/${p.path}`) }));
  const newest = docs.map((d) => d.lastModified).filter(Boolean).sort().at(-1);
  return [
    { url: SITE, lastModified: lastModified("app/(home)/page.tsx") },
    { url: `${SITE}/docs`, lastModified: newest },
    ...docs,
  ];
}
