import { readFileSync } from "node:fs";
import { join } from "node:path";

export type Page = { slug: string; title: string; body: string };

// content/docs.json is written by scripts/sync-docs.mjs before dev and build
export const pages: Page[] = JSON.parse(readFileSync(join(process.cwd(), "content/docs.json"), "utf8"));

export const REPO = "Adityaadpandey/zerd-harness";
