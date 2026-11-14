import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { DocsBody, DocsDescription, DocsPage, DocsTitle } from "fumadocs-ui/layouts/docs/page";
import { source } from "@/lib/source";
import { getMDXComponents } from "@/components/mdx";
import { JsonLd } from "@/lib/jsonld";
import { NAME, SITE } from "@/lib/site";
import { DOC_SEO, pageMeta } from "@/lib/seo";
import { lastModified } from "@/lib/lastmod";

type Props = { params: Promise<{ slug: string[] }> };

export default async function Page({ params }: Props) {
  const page = source.getPage((await params).slug);
  if (!page) notFound();
  const MDX = page.data.body;
  const seo = DOC_SEO[page.slugs.join("/")];
  const url = `${SITE}${page.url}`;
  return (
    <DocsPage toc={page.data.toc} full={page.data.full}>
      <JsonLd
        data={{
          "@context": "https://schema.org",
          "@graph": [
            {
              "@type": "TechArticle", headline: seo?.title ?? page.data.title, description: seo?.description ?? page.data.description,
              url, mainEntityOfPage: url, inLanguage: "en", image: `${SITE}/og/${page.slugs.join("/")}`, dateModified: lastModified(`content/docs/${page.path}`),
              keywords: seo?.keywords?.join(", "), isPartOf: { "@id": `${SITE}/#site` }, about: { "@id": `${SITE}/#app` },
              author: { "@id": `${SITE}/#org` }, publisher: { "@id": `${SITE}/#org` },
            },
            {
              "@type": "BreadcrumbList",
              itemListElement: [
                { "@type": "ListItem", position: 1, name: NAME, item: SITE },
                { "@type": "ListItem", position: 2, name: "Docs", item: `${SITE}/docs` },
                { "@type": "ListItem", position: 3, name: page.data.title, item: url },
              ],
            },
          ],
        }}
      />
      <DocsTitle>{page.data.title}</DocsTitle>
      <DocsDescription>{page.data.description}</DocsDescription>
      <DocsBody>
        <MDX components={getMDXComponents()} />
      </DocsBody>
    </DocsPage>
  );
}

export function generateStaticParams() {
  return source.generateParams();
}

export async function generateMetadata({ params }: Props): Promise<Metadata> {
  const page = source.getPage((await params).slug);
  if (!page) notFound();
  // Hand-tuned search copy wins; the frontmatter title and description are the fallback for a new page.
  const seo = DOC_SEO[page.slugs.join("/")];
  return pageMeta({ path: page.url, type: "article", title: seo?.title ?? page.data.title, description: seo?.description ?? page.data.description ?? "", keywords: seo?.keywords, image: `/og/${page.slugs.join("/")}` });
}
