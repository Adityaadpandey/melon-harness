import defaultMdxComponents from "fumadocs-ui/mdx";
import type { MDXComponents } from "mdx/types";
import { Step, Steps } from "fumadocs-ui/components/steps";
import { Accordion, Accordions } from "fumadocs-ui/components/accordion";
import { Mermaid } from "@/components/mermaid";

export function getMDXComponents(components?: MDXComponents): MDXComponents {
  return {
    ...defaultMdxComponents,
    Mermaid, Step, Steps, Accordion, Accordions,
    // plain img: the docs images are static files without known sizes
    img: (props) => <img {...props} alt={props.alt ?? ""} loading="lazy" />,
    ...components,
  };
}
