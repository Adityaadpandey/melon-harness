import Link from "next/link";
import { REPO } from "@/lib/docs";
import { latestRelease } from "@/lib/latest";
import Logo from "./logo";
import Nav from "./nav";
import "./landing.css";
import Spotlight from "./spotlight";

// The landing page has its own nav and footer.
// The docs bring their own chrome.
export default async function HomeLayout({ children }: { children: React.ReactNode }) {
  const rel = await latestRelease();
  return (
    <div className="lp">
      <Spotlight />
      <Nav repo={REPO} />
      {children}
      <footer className="foot-lp">
        <div className="foot-card">
          <div className="foot-cta"><Link className="foot-pill" href="/docs/install">Try zerd on a small change</Link></div>
          <div className="foot-row">
            <div className="foot-left">
              <a href={`https://github.com/${REPO}`} aria-label="zerd on GitHub">
                <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M12 .5a11.5 11.5 0 0 0-3.64 22.41c.58.1.79-.25.79-.56v-2c-3.2.7-3.88-1.37-3.88-1.37-.52-1.33-1.28-1.69-1.28-1.69-1.04-.71.08-.7.08-.7 1.16.08 1.77 1.19 1.77 1.19 1.03 1.76 2.7 1.25 3.36.96.1-.75.4-1.25.73-1.54-2.55-.29-5.24-1.28-5.24-5.69 0-1.26.45-2.28 1.19-3.09-.12-.29-.52-1.46.11-3.05 0 0 .97-.31 3.17 1.18a11 11 0 0 1 5.78 0c2.2-1.49 3.17-1.18 3.17-1.18.63 1.59.23 2.76.11 3.05.74.81 1.19 1.83 1.19 3.09 0 4.42-2.7 5.4-5.27 5.68.41.36.78 1.06.78 2.14v3.17c0 .31.21.67.8.56A11.5 11.5 0 0 0 12 .5Z" /></svg>
              </a>
              <span>{rel ? `v${rel.version}` : "zerd"}</span>
            </div>
            <Logo />
            <nav className="foot-right" aria-label="Footer">
              <Link href="/docs/models-and-routing">Models</Link>
              <a href={`https://github.com/${REPO}/releases`}>Releases</a>
              <Link href="/docs/license">License</Link>
              <a href="/llms.txt">llms.txt</a>
              <Link href="/docs/what-zerd-does-that-others-leave-to-the-model">Why zerd</Link>
            </nav>
          </div>
        </div>
        <div className="foot-word" aria-hidden="true"><span>zerd</span></div>
      </footer>
    </div>
  );
}
