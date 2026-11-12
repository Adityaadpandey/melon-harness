import Link from "next/link";
import { REPO } from "@/lib/docs";

// The landing page keeps the site's own nav and footer. The docs bring their own.
export default function HomeLayout({ children }: { children: React.ReactNode }) {
  return (
    <>
      <header className="nav">
        <div className="nav-in">
          <Link href="/" className="logo">zerd<i className="logo-tick" aria-hidden /></Link>
          <nav>
            <Link href="/docs/try-it-in-two-minutes">Docs</Link>
            <Link href="/docs/models-and-routing">Models</Link>
            <a href={`https://github.com/${REPO}/releases`}>Releases</a>
            <a className="nav-cta" href={`https://github.com/${REPO}`}>GitHub ↗</a>
          </nav>
        </div>
      </header>
      {children}
      <footer className="foot">
        <div className="foot-in">
          <div>
            <Link href="/" className="logo">zerd<i className="logo-tick" aria-hidden /></Link>
            <p>The terminal coding agent that proves its work.</p>
          </div>
          <div className="foot-cols">
            <div><h4>Product</h4><Link href="/docs/try-it-in-two-minutes">Get started</Link><Link href="/docs/what-makes-it-different">Why zerd</Link><Link href="/docs/models-and-routing">Models</Link></div>
            <div><h4>Project</h4><a href={`https://github.com/${REPO}`}>GitHub</a><a href={`https://github.com/${REPO}/releases`}>Releases</a><Link href="/docs/license">License</Link></div>
          </div>
        </div>
      </footer>
    </>
  );
}
