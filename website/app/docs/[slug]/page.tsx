import type { Metadata } from "next";
import { notFound } from "next/navigation";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeSlug from "rehype-slug";
import { pages } from "@/lib/docs";

export const dynamicParams = false;

export function generateStaticParams() {
  return pages.map((p) => ({ slug: p.slug }));
}

export async function generateMetadata({ params }: { params: Promise<{ slug: string }> }): Promise<Metadata> {
  const { slug } = await params;
  return { title: pages.find((p) => p.slug === slug)?.title };
}

export default async function DocPage({ params }: { params: Promise<{ slug: string }> }) {
  const { slug } = await params;
  const i = pages.findIndex((p) => p.slug === slug);
  if (i < 0) notFound();
  const { title, body } = pages[i];
  const prev = pages[i - 1];
  const next = pages[i + 1];
  return (
    <>
      <h1>{title}</h1>
      <Markdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeSlug]}>{body}</Markdown>
      <nav className="pager">
        {prev ? <a href={`/docs/${prev.slug}`}>← {prev.title}</a> : <span />}
        {next ? <a href={`/docs/${next.slug}`}>{next.title} →</a> : <span />}
      </nav>
    </>
  );
}
