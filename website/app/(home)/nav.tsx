import Link from "next/link";
import Logo from "./logo";

// Dark glass bar pinned to the top. No scroll listener, no entrance animation.
export default function Nav({ repo }: { repo: string }) {
  return (
    <header className="sk-nav">
      <div className="sk-nav-in">
        <Logo />
        <nav aria-label="Main">
          <Link href="/docs/try-it-in-two-minutes">Docs</Link>
          <Link href="/docs/models-and-routing">Models</Link>
          <a href={`https://github.com/${repo}/releases`}>Releases</a>
          <a className="sk-pill" href="#install">Install</a>
        </nav>
      </div>
    </header>
  );
}
