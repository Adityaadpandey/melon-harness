import Link from "next/link";
import { REPO } from "@/lib/docs";
import Nav from "./nav";

// The landing page has its own frosted nav and a footer that sits on the last clouds.
// The docs bring their own chrome.
export default function HomeLayout({ children }: { children: React.ReactNode }) {
  return (
    <div className="sky-root">
      <Nav repo={REPO} />
      {children}
      <footer className="sk-foot">
        <div className="sk-foot-in">
          <div>
            <Link href="/" className="sk-logo">zerd<i aria-hidden /></Link>
            <p>The terminal coding agent that proves its work.</p>
            <p className="models">Works with Claude, GPT, Codex, DeepSeek and local models, on macOS, Linux and Windows. <Link href="/docs/models-and-routing">How models are routed</Link>.</p>
          </div>
          <div className="sk-foot-cols">
            <div><h4>Product</h4><Link href="/docs/try-it-in-two-minutes">Get started</Link><Link href="/docs/what-makes-it-different">Why zerd</Link><Link href="/docs/models-and-routing">Models</Link></div>
            <div><h4>Project</h4><a href={`https://github.com/${REPO}`}>GitHub</a><a href={`https://github.com/${REPO}/releases`}>Releases</a><Link href="/docs/license">License</Link></div>
          </div>
        </div>
      </footer>
    </div>
  );
}
