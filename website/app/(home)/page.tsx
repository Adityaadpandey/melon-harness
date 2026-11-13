import Link from "next/link";
import { latestRelease } from "@/lib/latest";
import { JsonLd } from "@/lib/jsonld";
import CopyCmd from "./copy-cmd";
import Demo from "./demo";
import RunLog from "./runlog";

const INSTALL = "curl -fsSL https://zerd.si/install.sh | sh";

// The eight defects from the webhook benchmark: what broke, and the fix.
const BUGS: [string, string][] = [
  ["Webhooks never reached the database", "Quoted the SQL identifiers"],
  ["A captured payment ended up FAILED", "Row lock, rerun as 25 races"],
  ["Retries fired 5.5 hours late", "UTC timestamps"],
  ["The outbox stopped draining", "Bounded timeout"],
  ["Events stuck in RECEIVED forever", "Recovery sweep covers it"],
  ["The same event queued twice", "Claim inside the transaction"],
  ["A crash marked work IGNORED", "Fixed, tested with real SIGKILL"],
  ["BullMQ refused the job", "Dropped the colon ids"],
];

const VERSUS: [string, string, string][] = [
  ["Real PostgreSQL", "Yes", "No"],
  ["Real Redis", "Yes", "No"],
  ["Worker crash", "kill -9", "Simulated"],
  ["Concurrent claims", "25 real races", "In-memory"],
  ["Outages", "Exercised", "Simulated"],
  ["Scenarios", "15 of 15 run", "15 of 15 attempted"],
];

// A worry a team has, and the part of zerd that answers it.
const NEEDS: [string, string][] = [
  ["Too much to review", "A second model and a verifier read the diff first."],
  ["Green tests, broken change", "Checks run on what changed. Dropped parameters get caught."],
  ["Invented files and lines", "Every cited path is looked up before you see it."],
  ["Quota ran out mid-task", "Fails over to the next provider."],
  ["Reads .env, touches other repos", "Blocked in code and by an OS sandbox."],
  ["Starts from zero every time", "Corrections become lessons in your AGENTS.md."],
  ["The bill grows", "Sends only new lines. 87% cache hits."],
];

const PIPE: [string, string, string?][] = [
  ["Edit", "The agent changes your files."],
  ["Check", "Tests and lint on the changed files.", "npx tsc --noEmit   ✓ Ran"],
  ["Review", "A different model reads the real diff."],
  ["Refute", "A verifier tries to disprove each finding."],
  ["Look up", "Every cited file and line is checked on disk."],
  ["Report", "What ran, what held, what stayed unverified.", "verified  1 files · 2 checks ok · /undo reverts"],
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
            <img src="/hero/hero.webp" alt="" width={1672} height={941} fetchPriority="high" decoding="async" />
          </picture>
        </div>
        <div className="hero-copy">
          <h1>Stop re-checking your AI’s work.</h1>
          <p className="lead">zerd tests on your real database, has a second model review the diff, and says what it could not verify.</p>
          <CopyCmd cmd={INSTALL} />
          <p className="hero-links">
            <Link href="/docs/try-it-in-two-minutes">Try it on a small change</Link>
            <a href="#versus">See it against Claude Code</a>
          </p>
          <ul className="gains">
            <li><b>Review less.</b> Findings are argued over before you see them.</li>
            <li><b>Catch it before production.</b> Crashes and races hit real Postgres and Redis.</li>
            <li><b>Keep your options open.</b> Any model. Quota out? It switches.</li>
          </ul>
        </div>
        <RunLog />
      </section>

      <div className="after">
      <section className="lift wrap" aria-label="A real recording of zerd">
        <Demo />
      </section>
      <section className="sec wrap split" id="bugs">
        <div className="sec-head">
          <h2>Eight bugs a mock never shows</h2>
          <p>15 attack scenarios on real Postgres and Redis. Each failure reproduced, fixed, rerun.</p>
        </div>
        <div className="bugs glow" role="table" aria-label="Defects found">
          <div className="bug-h" role="row"><span role="columnheader">What broke</span><span role="columnheader">The fix</span></div>
          {BUGS.map(([a, c]) => (
            <div className="bug" role="row" key={a}><strong role="cell">{a}</strong><span role="cell" className="fix">{c}</span></div>
          ))}
        </div>
      </section>

      <section className="sec wrap" id="versus">
        <div className="sec-head">
          <h2>Same task. Claude Code stayed in the service layer.</h2>
          <p>zerd on MiniMax M3 vs Claude Code on Sonnet 5.5. Claude Code found real bugs too, and said plainly it never touched real infrastructure.</p>
        </div>
        <div className="vs glow" role="table" aria-label="zerd and Claude Code compared">
          <div className="vs-h" role="row"><span role="columnheader"></span><span role="columnheader">zerd, MiniMax M3</span><span role="columnheader">Claude Code, Sonnet 5.5</span></div>
          {VERSUS.map(([k, z, c]) => (
            <div className="vs-r" role="row" key={k}><span role="rowheader">{k}</span><span role="cell" className="yes">{z}</span><span role="cell">{c}</span></div>
          ))}
        </div>
        <p className="caveat">One project, one run each. This shows depth of verification, not a model ranking. zerd’s own open risks: tenant isolation, replay auditing, out-of-order payments.</p>
        <p className="caveat">Earlier 11-task run: 10 of 11 each. zerd used fewer tokens (1.01M vs 1.31M) but made more calls and was slower. Claude Code deleted <code>.git</code> when asked to wipe history.</p>
      </section>

      <section className="wrap story">
        <div className="story-side">
          <h2>No “done” without evidence</h2>
          <p>Six steps after every change. Most agents skip Refute and Look up.</p>
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

      <section className="sec wrap split">
        <div className="sec-head">
          <h2>What teams worry about</h2>
        </div>
        <dl className="needs">
          {NEEDS.map(([q, a]) => (
            <div key={q}><dt>{q}</dt><dd>{a}</dd></div>
          ))}
        </dl>
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
