import { createMDX } from "fumadocs-mdx/next";

/** @type {import('next').NextConfig} */
const config = {
  reactStrictMode: true,
  // /docs/quickstart.md serves that page as plain markdown for agents
  rewrites: async () => [{ source: "/docs/:slug*.md", destination: "/md/:slug*" }],
};

export default createMDX()(config);
