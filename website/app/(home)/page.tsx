import Link from "next/link";
import { latestRelease } from "@/lib/latest";
import { JsonLd } from "@/lib/jsonld";
import CopyCmd from "./copy-cmd";
import Demo from "./demo";
import RunLog from "./runlog";

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

const FAQ = [
  ["What is zerd?", "A terminal coding agent that runs checks on the files it changed, has a second model review the real diff, and verifies every file path and line it mentions against your tree."],
  ["How do I install zerd?", "On macOS or Linux run: curl -fsSL https://zerd.si/install.sh | sh. On Windows PowerShell run: irm https://zerd.si/install.ps1 | iex. It is a single binary with no runtime to set up."],
  ["Which models does zerd work with?", "Claude, GPT, Codex, DeepSeek and local models. If a provider's quota runs out, zerd fails over to the next one and tells you."],
  ["How is zerd different from other coding agents?", "Instead of saying “done”, it reports the checks that ran, a review of the actual diff, and which claims were verified. Safety rules are enforced in code (confined tools and an OS sandbox), not just in the prompt."],
  ["Is zerd free and open source?", "It is free to run, including for commercial use. The source is on GitHub but it is source-available, not open source: you can read it, but not copy, modify or redistribute it."],
] as const;

export default async function Home() {
  const rel = await latestRelease();
  return (
    <main>
      <JsonLd data={{ "@context": "https://schema.org", "@type": "FAQPage", mainEntity: FAQ.map(([q, a]) => ({ "@type": "Question", name: q, acceptedAnswer: { "@type": "Answer", text: a } })) }} />

      <section className="hero">
        <div className="hero-img" aria-hidden="true">
          <picture>
            <source srcSet="/hero/hero.avif" type="image/avif" />
            <img src="/hero/hero.webp" alt="" draggable={false} width={1672} height={941} fetchPriority="high" decoding="async" />
          </picture>
        </div>
        <div className="hero-copy">
          <p className="eyebrow">The coding agent that shows its proof</p>
          <h1>Ship AI-written code without re-checking it.</h1>
          <p className="lead">Other agents say “done.” zerd shows what it tested, has a second AI review the real changes, and flags anything it couldn’t verify.</p>
          <CopyCmd cmd={INSTALL} />
          <p className="hero-links">
            <Link href="/docs/quickstart">Try it in two minutes</Link>
            <a href="#proof">See it beat a mock test</a>
          </p>
          <p className="hero-fine">Free to run · macOS, Linux, Windows · Claude, GPT, DeepSeek or local models</p>
        </div>
        <RunLog />
      </section>

      <div className="after">
      <section className="lift wrap" aria-label="A real recording of zerd">
        <Demo />
      </section>
      <section className="sec wrap story" id="how">
        <div className="story-side">
          <p className="eyebrow">How it works</p>
          <h2>Every change comes with receipts</h2>
          <p>Six steps run on their own after each edit. You read the result, not the whole diff.</p>
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
          <p className="eyebrow">The proof</p>
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
          <p className="eyebrow">Get started</p>
          <h2>Try it on one small change</h2>
          <p>One binary, no setup, two minutes. If it doesn’t save you review time, /undo puts everything back.</p>
        </div>
        <CopyCmd cmd={INSTALL} />
        <p className="hero-fine">Windows: <code>irm https://zerd.si/install.ps1 | iex</code></p>
        <p className="hero-links">
          <Link href="/docs/quickstart">Get started</Link>
          <Link href="/docs/what-zerd-does-that-others-leave-to-the-model">Why zerd</Link>
          {rel && <span>v{rel.version}</span>}
        </p>
      </section>
      </div>
    </main>
  );
}
