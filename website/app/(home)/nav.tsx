import Link from "next/link";
import Logo from "./logo";

// A floating pill, centred, that sits over the hero and picks up a blur as it passes the page.
export default function Nav({ repo }: { repo: string }) {
  return (
    <header className="nav">
      <div className="nav-pill">
        <Logo />
        <nav aria-label="Main">
          <a href="#how">How it works</a>
          <a href="#proof">Proof</a>
          <Link href="/docs/try-it-in-two-minutes">Docs</Link>
          <a href={`https://github.com/${repo}`}>GitHub</a>
        </nav>
        <a className="nav-install" href="#install">Install</a>
      </div>
    </header>
  );
}
