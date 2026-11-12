import type { Metadata } from "next";
import Link from "next/link";
import { notFound } from "next/navigation";
import type { ReactNode } from "react";
import { MarkdownAsync as Markdown, type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeSlug from "rehype-slug";
import rehypeAutolink from "rehype-autolink-headings";
import rehypeShiki from "@shikijs/rehype";
import { remarkAlert } from "remark-github-blockquote-alert";
import { pages, ordered, groupOf, readingMinutes } from "@/lib/docs";
import { headingsOf } from "@/lib/headings";
import CopyButton from "../CopyButton";
import Toc from "../Toc";

export const dynamicParams = false;

export function generateStaticParams() {
  return pages.map((p) => ({ slug: p.slug }));
}

export async function generateMetadata({ params }: { params: Promise<{ slug: string }> }): Promise<Metadata> {
  const { slug } = await params;
  return { title: pages.find((p) => p.slug === slug)?.title };
}

// flatten react children back into the plain text of a code block
function textOf(n: ReactNode): string {
  if (typeof n === "string" || typeof n === "number") return String(n);
  if (Array.isArray(n)) return n.map(textOf).join("");
  if (n && typeof n === "object" && "props" in n) return textOf((n as any).props.children);
  return "";
}

const components: Components = {
  pre({ children, className, style }) {
    const code: any = Array.isArray(children) ? children[0] : children;
    const lang = /language-(\S+)/.exec(code?.props?.className ?? "")?.[1] ?? "text";
    return (
      <div className="code">
        <div className="code-bar">
          <span>{lang}</span>
          <CopyButton text={textOf(code).replace(/\n$/, "")} />
        </div>
        <pre className={className} style={style}>{children}</pre>
      </div>
    );
  },
  table({ children }) {
    return <div className="tbl"><table>{children}</table></div>;
  },
};

export default async function DocPage({ params }: { params: Promise<{ slug: string }> }) {
  const { slug } = await params;
  const i = ordered.findIndex((p) => p.slug === slug);
  if (i < 0) notFound();
  const { title, body } = ordered[i];
  const prev = ordered[i - 1];
  const next = ordered[i + 1];
  return (
    <div className="dgrid">
      <article className="prose">
        <header className="ahead">
          <div className="crumb">
            <Link href="/docs">{groupOf(slug)}</Link>
            <span>{readingMinutes(body)} min read</span>
          </div>
          <h1>{title}</h1>
        </header>
        <Markdown remarkPlugins={[remarkGfm, remarkAlert]}
          rehypePlugins={[
            rehypeSlug,
            [rehypeAutolink, { behavior: "append", properties: { className: "anchor", ariaLabel: "Link to this section" }, content: { type: "text", value: "#" } }],
            [rehypeShiki, { theme: "github-dark-default", fallbackLanguage: "text", addLanguageClass: true }],
          ]} components={components}>{body}</Markdown>
        <nav className="pager">
          {prev ? (
            <Link href={`/docs/${prev.slug}`}><small>Previous</small><span>{prev.title}</span></Link>
          ) : <span />}
          {next ? (
            <Link href={`/docs/${next.slug}`} className="next"><small>Next</small><span>{next.title}</span></Link>
          ) : <span />}
        </nav>
      </article>
      <Toc headings={headingsOf(body)} />
    </div>
  );
}
