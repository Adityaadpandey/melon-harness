import defaultMdxComponents from "fumadocs-ui/mdx";
import type { MDXComponents } from "mdx/types";
import { Mermaid } from "@/components/mermaid";

export function getMDXComponents(components?: MDXComponents): MDXComponents {
  return {
    ...defaultMdxComponents,
    Mermaid,
    // plain img: the docs images are static files without known sizes
    img: (props) => <img {...props} alt={props.alt ?? ""} loading="lazy" />,
    ...components,
  };
}
