import Link from "next/link";
import { REPO } from "@/lib/docs";
import { latestRelease } from "@/lib/latest";
import Logo from "./logo";
import Nav from "./nav";

// The landing page has its own frosted nav and a footer that sits on the last clouds.
// The docs bring their own chrome.
export default async function HomeLayout({ children }: { children: React.ReactNode }) {
  const rel = await latestRelease();
  return (
    <div className="sky-root">
      <Nav repo={REPO} />
      {children}
      <footer className="sk-foot">
        <div className="sk-foot-in">
          <div>
            <Logo />
            <p>The terminal coding agent that proves its work.</p>
            <p className="models">Works with Claude, GPT, Codex, DeepSeek and local models, on macOS, Linux and Windows. <Link href="/docs/models-and-routing">How models are routed</Link>.</p>
          </div>
          <div className="sk-foot-cols">
            <div><h4>Product</h4><Link href="/docs/try-it-in-two-minutes">Get started</Link><Link href="/docs/what-makes-it-different">Why zerd</Link><Link href="/docs/models-and-routing">Models</Link></div>
            <div><h4>Project</h4><a href={`https://github.com/${REPO}`}>GitHub</a><a href={`https://github.com/${REPO}/releases`}>Releases</a><Link href="/docs/license">License</Link></div>
          </div>
        </div>
        <div className="sk-foot-end">
          <div className="r-bars" aria-hidden="true" />
          <p>zerd{rel ? ` v${rel.version}` : ""}. One native binary, about 11 MB, for macOS, Linux and Windows.</p>
        </div>
      </footer>
    </div>
  );
}
