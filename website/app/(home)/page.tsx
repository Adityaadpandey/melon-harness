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
  ["Edit", "zerd changes your files."],
  ["Test", "Runs tests and lint on what changed.", "npx tsc --noEmit   ✓ Ran"],
  ["Review", "A second AI reads the actual changes."],
  ["Challenge", "A third pass tries to prove each finding wrong."],
  ["Verify", "Every file and line it mentions is checked to exist."],
  ["Report", "What ran, what passed, what is still unverified.", "verified  1 files · 2 checks ok · /undo reverts"],
];

const FAQ = [
  ["What is zerd?", "A terminal coding agent that runs checks on the files it changed, has a second model review the real diff, and verifies every file path and line it mentions against your tree."],
  ["How do I install zerd?", "On macOS or Linux run: curl -fsSL https://zerd.si/install.sh | sh. On Windows PowerShell run: irm https://zerd.si/install.ps1 | iex. It is a single binary with no runtime to set up."],
  ["Which models does zerd work with?", "Claude, GPT, Codex, DeepSeek and local models. If a provider's quota runs out, zerd fails over to the next one and tells you."],
  ["How is zerd different from other coding agents?", "Instead of saying “done”, it reports the checks that ran, a review of the actual diff, and which claims were verified. Safety rules are enforced in code (confined tools and an OS sandbox), not just in the prompt."],
  ["Is zerd free and open source?", "Yes, the code is on GitHub and the license is in the docs."],
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
          <h1>Stop re-checking your AI’s work.</h1>
          <p className="lead">zerd is an AI coding agent for your terminal. After every change it checks its own work, and it tells you what it could not verify.</p>
          <CopyCmd cmd={INSTALL} />
          <p className="hero-links">
            <Link href="/docs/try-it-in-two-minutes">Try it on a small change</Link>
            <a href="#proof">See the proof</a>
          </p>
          <ul className="gains">
            <li><b>Review less.</b> A second AI double-checks the change before you see it.</li>
            <li><b>Catch bugs early.</b> It tests against a real database, not a fake one.</li>
            <li><b>Use any model.</b> If one runs out of quota, it switches to the next.</li>
          </ul>
        </div>
        <RunLog />
      </section>

      <div className="after">
      <section className="lift wrap" aria-label="A real recording of zerd">
        <Demo />
      </section>
      <section className="sec wrap" id="proof">
        <div className="sec-head">
          <h2>Same task, tested deeper</h2>
          <p>We gave zerd and Claude Code the same job: 15 attack scenarios on a payments webhook. zerd found and fixed 8 real bugs. Claude Code found real bugs too, but never ran against real infrastructure.</p>
        </div>
        <div className="vs glow" role="table" aria-label="zerd and Claude Code compared">
          <div className="vs-h" role="row"><span role="columnheader"></span><span role="columnheader">zerd, MiniMax M3</span><span role="columnheader">Claude Code, Sonnet 5.5</span></div>
          {VERSUS.map(([k, z, c]) => (
            <div className="vs-r" role="row" key={k}><span role="rowheader">{k}</span><span role="cell" className="yes">{z}</span><span role="cell">{c}</span></div>
          ))}
        </div>
        <p className="caveat">One project, one run each. This shows how deeply each tool verifies, not which model is better.</p>
      </section>

      <section className="wrap story" id="how">
        <div className="story-side">
          <h2>It never just says “done”</h2>
          <p>After every change, zerd runs these six steps and shows you the result.</p>
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

      <section className="sec wrap install" id="install">
        <div className="sec-head">
          <h2>Try it on a small change</h2>
          <p>One binary. No runtime.</p>
        </div>
        <CopyCmd cmd={INSTALL} />
        <p className="hero-fine">Windows: <code>irm https://zerd.si/install.ps1 | iex</code></p>
        <p className="hero-links">
          <Link href="/docs/try-it-in-two-minutes">Get started</Link>
          <Link href="/docs/what-makes-it-different">Why zerd</Link>
          {rel && <span>v{rel.version}</span>}
        </p>
      </section>
      </div>
    </main>
  );
}
