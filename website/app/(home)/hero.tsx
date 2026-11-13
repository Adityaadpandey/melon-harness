import Link from "next/link";
import Install from "./install";

// The hero is the desk and the printer. Everything below the slot is the receipt, which feeds out
// from under this block when the page loads. The block has its own solid background so the paper
// stays hidden until it has cleared the slot.
export default function Hero() {
  return (
    <section className="z-hero">
      <div className="z-wrap z-hero-in">
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
      <div className="z-slot" aria-hidden="true"><i /></div>
    </section>
  );
}
