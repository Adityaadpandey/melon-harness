import Link from "next/link";
import { latestRelease } from "@/lib/latest";
import { JsonLd } from "@/lib/jsonld";
import CopyCmd from "./copy-cmd";
import Demo from "./demo";
import { FAQ } from "@/lib/faq";

const INSTALL = "curl -fsSL https://zerd.si/install.sh | sh";

const VERSUS: [string, string, string][] = [
  ["Real PostgreSQL", "Yes", "No"],
  ["Real Redis", "Yes", "No"],
  ["Worker crash", "kill -9", "Simulated"],
  ["Concurrent claims", "25 real races", "In-memory"],
  ["Outages", "Exercised", "Simulated"],
  ["Scenarios", "15 of 15 run", "15 of 15 attempted"],
];

const PIPE: [string, string, string?][] = [
  ["It edits", "zerd makes the change in your files."],
  ["It tests", "Tests and lint run on exactly what changed.", "npx tsc --noEmit   ✓ Ran"],
  ["A second AI reviews", "A different model reads the real diff, not a summary."],
  ["A third pass objects", "A verifier tries to prove each finding wrong, so you only see the ones that hold."],
  ["It checks its own claims", "Every file and line it mentions is looked up on disk. No invented paths."],
  ["You get the receipt", "What ran, what passed, what is still unverified.", "verified  1 files · 2 checks ok · /undo reverts"],
];

export default async function Home() {
  const rel = await latestRelease();
  return (
    <main>
      <JsonLd data={{ "@context": "https://schema.org", "@type": "FAQPage", mainEntity: FAQ.map(([q, a]) => ({ "@type": "Question", name: q, acceptedAnswer: { "@type": "Answer", text: a } })) }} />

      <section className="hero">
        <div className="hero-img" aria-hidden="true">
          <picture>
            <source srcSet="/hero/hero.avif" type="image/avif" />
            <img src="/hero/hero.webp" alt="" draggable={false} width={2560} height={1441} fetchPriority="high" decoding="async" />
          </picture>
          <video autoPlay muted loop playsInline preload="auto" poster="/hero/hero-poster.webp" width={1280} height={662}>
            <source src="/hero/hero.webm" type="video/webm" />
            <source src="/hero/hero.mp4" type="video/mp4" />
          </video>
        </div>
        <div className="hero-copy">
          <h1>Your agent says “done.” zerd shows the proof.</h1>
          <p className="lead">zerd is a terminal AI coding agent. It runs tests on what it changed, has a second AI read the real diff, and tells you what it couldn’t verify. You read a short receipt, not the whole change.</p>
          <CopyCmd cmd={INSTALL} />
          <p className="hero-links">
            <Link href="/docs/quickstart">Try it in two minutes</Link>
            <a href="#proof">See it beat a mock test</a>
          </p>
          <p className="hero-fine">Free to run · macOS, Linux, Windows · Claude, GPT, DeepSeek or local models</p>
        </div>
      </section>

      <div className="after">
      <section className="lift wrap" aria-label="A real recording of zerd">
        <Demo />
      </section>
      <section className="sec wrap story" id="how">
        <div className="story-side">
          <h2>Every change comes with receipts</h2>
          <p>Six steps run on their own after every edit. None of them wait for you.</p>
        </div>
        <ol className="story-steps">
          {PIPE.map(([t, d, ev], i) => (
            <li key={t}>
              <span className="story-n">{i + 1}</span>
              <h3>{t}</h3>
              <p>{d}</p>
              {ev && <code className="glow">{ev}</code>}
            </li>
          ))}
        </ol>
      </section>

      <section className="sec wrap" id="proof">
        <div className="sec-head">
          <h2>It finds what mock tests can’t</h2>
          <p>Same payments project, 15 attack scenarios. zerd ran them on real Postgres and Redis and found 12 real bugs. Claude Code found real bugs too, but never touched real infrastructure.</p>
        </div>
        <div className="vs glow" role="table" aria-label="zerd and Claude Code compared">
          <div className="vs-h" role="row"><span role="columnheader"></span><span role="columnheader">zerd, MiniMax M3</span><span role="columnheader">Claude Code, Sonnet 5.5</span></div>
          {VERSUS.map(([k, z, c]) => (
            <div className="vs-r" role="row" key={k}><span role="rowheader">{k}</span><span role="cell" className="yes">{z}</span><span role="cell">{c}</span></div>
          ))}
        </div>
        <p className="caveat">One project, one run each. This shows how deeply each tool verifies, not which model is better.</p>
      </section>

      <section className="sec wrap install" id="install">
        <div className="sec-head">
          <h2>Try it on one small change</h2>
          <p>One binary, no setup, two minutes. If it doesn’t save you review time, /undo puts everything back.</p>
        </div>
        <CopyCmd cmd={INSTALL} />
        <p className="hero-fine">Windows: <code>irm https://zerd.si/install.ps1 | iex</code></p>
        <p className="hero-links">
          <Link href="/docs/quickstart">Get started</Link>
          <Link href="/docs/verification">Why zerd</Link>
          {rel && <span>v{rel.version}</span>}
        </p>
      </section>
      <section className="sec wrap faq" id="faq">
        <div className="sec-head">
          <p className="eyebrow">Questions</p>
          <h2>Common questions about zerd</h2>
        </div>
        <div className="faq-list">
          {FAQ.map(([q, a]) => (
            <details key={q}>
              <summary>{q}</summary>
              <p>{a}</p>
            </details>
          ))}
        </div>
      </section>
      </div>
    </main>
  );
}
