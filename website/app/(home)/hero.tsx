import Link from "next/link";
import Install from "./install";
import Receipt from "./receipt";

export default function Hero() {
  return (
    <section className="z-hero">
      <div className="z-wrap z-hero-grid">
        <div className="z-hero-copy">
          <h1>Most agents say done. <span>zerd shows the receipt.</span></h1>
          <p className="z-sub">
            zerd is a coding agent for your terminal. Before it tells you a task is finished, it runs the tests that cover the change,
            has a second model read the diff, and checks that every file it mentions exists.
          </p>
          <Install />
          <p className="z-meta">
            macOS, Linux, Windows. Works with Claude, GPT, Codex, DeepSeek or a local model.{" "}
            <Link href="/docs/try-it-in-two-minutes">Two-minute start</Link>
          </p>
        </div>
        <Receipt />
      </div>
    </section>
  );
}
