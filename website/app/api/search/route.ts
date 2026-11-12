import { source } from "@/lib/source";
import { createFromSource } from "fumadocs-core/search/server";

// Exported once at build time and served as a static file.
export const revalidate = false;
export const { staticGET: GET } = createFromSource(source, { language: "english" });
