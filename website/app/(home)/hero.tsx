import Link from "next/link";
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
          <p className="p-eyebrow">terminal coding agent</p>
          <h1 className="p-h1">
            <span className="l1">Most agents say done.</span> <span className="l2">zerd shows the receipt.</span>
          </h1>
          <p className="p-sub">
            After it edits your code, zerd runs the checks that cover your changed files, has a second model review the real diff,
            and looks up every file path it mentions.
          </p>
          <div className="p-cta">
            <a className="p-btn" href="#install">Install zerd</a>
            <Link className="p-ghost" href="/docs/try-it-in-two-minutes">Read the docs</Link>
          </div>
        </div>
        <div className="p-rail">
          <span>one native binary, about 11 mb</span>
          <span>macos · linux · windows</span>
        </div>
      </div>

      <div className="wrap p-after">
        <Demo />
      </div>
    </section>
  );
}
