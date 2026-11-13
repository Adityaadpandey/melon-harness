import Link from "next/link";
import { REPO } from "@/lib/docs";
import { latestRelease } from "@/lib/latest";
import Logo from "./logo";
import Nav from "./nav";

export default async function HomeLayout({ children }: { children: React.ReactNode }) {
  const rel = await latestRelease();
  return (
    <div className="z-root">
      <Nav repo={REPO} />
      {children}
      <footer className="z-foot">
        <div className="z-wrap z-foot-in">
          <div>
            <Logo />
            <p>A coding agent that shows its receipt. Open source, one native binary.</p>
          </div>
          <div className="z-foot-links">
            <Link href="/docs/try-it-in-two-minutes">Get started</Link>
            <Link href="/docs/what-makes-it-different">What makes it different</Link>
            <Link href="/docs/models-and-routing">Models</Link>
            <a href={`https://github.com/${REPO}`}>GitHub</a>
            <a href={`https://github.com/${REPO}/releases`}>Releases{rel ? ` (v${rel.version})` : ""}</a>
            <Link href="/docs/license">License</Link>
          </div>
        </div>
      </footer>
    </div>
  );
}
