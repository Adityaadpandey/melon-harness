import { defineDocs } from "fumadocs-mdx/macro";
import { loader } from "fumadocs-core/source";

// content/docs is hand-written MDX, one file per page, ordered by content/docs/meta.json
const docs = defineDocs({ dir: "content/docs" });

export const source = loader({ baseUrl: "/docs", source: docs.toFumadocsSource() });
