import { pages } from "@/lib/docs";
import Sidebar from "./Sidebar";
import "./docs.css";

export default function DocsLayout({ children }: { children: React.ReactNode }) {
  return (
    <div className="docs">
      <Sidebar items={pages.map(({ slug, title }) => ({ slug, title }))} />
      <div className="dmain">{children}</div>
    </div>
  );
}
