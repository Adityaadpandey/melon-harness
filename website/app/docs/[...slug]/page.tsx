import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { DocsBody, DocsDescription, DocsPage, DocsTitle } from "fumadocs-ui/layouts/docs/page";
import { source } from "@/lib/source";
import { getMDXComponents } from "@/components/mdx";
import { JsonLd } from "@/lib/jsonld";
import { NAME, SITE } from "@/lib/site";

type Props = { params: Promise<{ slug: string[] }> };

export default async function Page({ params }: Props) {
  const page = source.getPage((await params).slug);
  if (!page) notFound();
  const MDX = page.data.body;
  return (
    <DocsPage toc={page.data.toc} full={page.data.full}>
      <JsonLd
        data={{
          "@context": "https://schema.org", "@type": "TechArticle", headline: page.data.title, description: page.data.description,
          url: `${SITE}${page.url}`, inLanguage: "en", isPartOf: { "@id": `${SITE}/#site` }, publisher: { "@type": "Organization", name: NAME, url: SITE },
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
  const { title, description } = page.data;
  return { title, description, alternates: { canonical: page.url }, openGraph: { type: "article", url: page.url, title, description }, twitter: { title, description } };
}
