import Link from "next/link";
import Logo from "./logo";

export default function Nav({ repo }: { repo: string }) {
  return (
    <header className="z-nav">
      <div className="z-nav-in">
        <Logo />
        <nav aria-label="Main">
          <Link href="/docs/try-it-in-two-minutes">Docs</Link>
          <a href={`https://github.com/${repo}`}>GitHub</a>
          <a href="#install">Install</a>
        </nav>
      </div>
    </header>
  );
}
