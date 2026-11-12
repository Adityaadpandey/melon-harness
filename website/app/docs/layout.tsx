import { groups } from "@/lib/docs";
import Sidebar from "./Sidebar";
import "./docs.css";

export default function DocsLayout({ children }: { children: React.ReactNode }) {
  const data = groups.map((g) => ({ title: g.title, pages: g.pages.map(({ slug, title }) => ({ slug, title })) }));
  return (
    <div className="docs">
      <Sidebar groups={data} />
      <div className="dmain">{children}</div>
    </div>
  );
}
