import Link from "next/link";
import Install from "./install";
import Demo from "./demo";
import "./poster.css";

// Hero: the artwork on the right, the words on the left over a dark scrim, the recorded run below.
// Deliberately static. No scroll effects, no entrance animations: it paints once and stays put.
export default function Hero() {
  return (
    <section className="poster-hero">
      <div className="poster">
        <div className="p-img" aria-hidden="true">
          <picture>
            <source srcSet="/hero/hero.avif" type="image/avif" />
            <img src="/hero/hero.webp" alt="" width={1672} height={941} fetchPriority="high" decoding="async" />
          </picture>
        </div>
        <div className="p-top">
          <h1 className="p-h1">
            <span className="l1">Most agents say done.</span> <span className="l2">zerd shows the receipt.</span>
          </h1>
          <p className="p-sub">Hand zerd a bug or a refactor. It runs the checks on what it changed, has a second model read the diff, and looks up every file path it cites. Works with Claude, GPT, Codex, DeepSeek or a local model.</p>
          <div className="p-cta">
            <Install />
          </div>
          <Link className="p-more" href="/docs/try-it-in-two-minutes">Get started in two minutes</Link>
        </div>
        <div className="p-rail">
          <span>one native binary · open source</span>
          <span>macos · linux · windows</span>
        </div>
      </div>

      <div className="wrap p-after">
        <ol className="proof">
          <li><span>01</span><h3>Runs the checks</h3><p>The ones that cover the files it just changed, not the whole repo.</p></li>
          <li><span>02</span><h3>Reviews the real diff</h3><p>A second model reads what actually changed, not what the agent says it changed.</p></li>
          <li><span>03</span><h3>Looks up every path</h3><p>Each file path in the answer is checked on disk before you see it.</p></li>
        </ol>
        <Demo />
      </div>
    </section>
  );
}
