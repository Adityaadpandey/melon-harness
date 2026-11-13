// Copies the few things the site serves straight from the repo root.
//   scripts/    -> content/install.*     (served at /install.sh and /install.ps1)
//   docs/assets -> public/docs/assets    (images the docs point at)
// The docs pages themselves are hand-written MDX in content/docs, they are not generated.
import { cpSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const site = join(dirname(fileURLToPath(import.meta.url)), "..");
const root = join(site, "..");

for (const f of ["install.sh", "install.ps1"]) cpSync(join(root, "scripts", f), join(site, "content", f));
mkdirSync(join(site, "public/docs"), { recursive: true });
cpSync(join(root, "docs/assets"), join(site, "public/docs/assets"), { recursive: true });
console.log("synced install scripts and doc assets");
