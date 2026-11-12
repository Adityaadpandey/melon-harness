import type { Metadata } from "next";
import Link from "next/link";
import { REPO } from "@/lib/docs";
import "./globals.css";

export const metadata: Metadata = {
  metadataBase: new URL("https://zerd.si"),
  title: { default: "zerd, the coding agent that proves its work", template: "%s · zerd" },
  description:
    "A terminal coding agent that runs checks on your changes, has a second model review the real diff, and verifies every file path it mentions.",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>
        <header className="nav">
          <Link href="/" className="logo">zerd</Link>
          <nav>
            <Link href="/docs/try-it-in-two-minutes">Docs</Link>
            <a href={`https://github.com/${REPO}`}>GitHub</a>
          </nav>
        </header>
        {children}
        <footer className="foot">
          <span>zerd.si</span>
          <a href={`https://github.com/${REPO}/releases`}>Releases</a>
          <Link href="/docs/license">License</Link>
        </footer>
      </body>
    </html>
  );
}
