import type { Metadata } from "next";
import Link from "next/link";
import { Bricolage_Grotesque, JetBrains_Mono } from "next/font/google";
import { REPO } from "@/lib/docs";
import "./globals.css";

const sans = Bricolage_Grotesque({ subsets: ["latin"], variable: "--font-sans" });
const mono = JetBrains_Mono({ subsets: ["latin"], variable: "--font-mono" });

export const metadata: Metadata = {
  metadataBase: new URL("https://zerd.si"),
  title: { default: "zerd, the coding agent that proves its work", template: "%s · zerd" },
  description:
    "A terminal coding agent that runs checks on your changes, has a second model review the real diff, and verifies every file path it mentions.",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className={`${sans.variable} ${mono.variable}`}>
      <body>
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
      </body>
    </html>
  );
}
