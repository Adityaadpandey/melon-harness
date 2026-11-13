import Link from "next/link";
import Logo from "./logo";

export default function Nav({ repo }: { repo: string }) {
  return (
    <header className="nav">
      <div className="nav-in">
        <Logo />
        <nav aria-label="Main">
          <a href="#bugs">Findings</a>
          <a href="#versus">Versus Claude Code</a>
          <Link href="/docs/try-it-in-two-minutes">Docs</Link>
          <a href={`https://github.com/${repo}`}>GitHub</a>
          <a className="nav-install" href="#install">Install</a>
        </nav>
      </div>
    </header>
  );
}
