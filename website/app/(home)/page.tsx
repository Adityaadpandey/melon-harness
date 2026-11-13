import Link from "next/link";
import { latestRelease } from "@/lib/latest";
import { JsonLd } from "@/lib/jsonld";
import CopyCmd from "./copy-cmd";
import Demo from "./demo";
import RunLog from "./runlog";

const INSTALL = "curl -fsSL https://zerd.si/install.sh | sh";

// The eight defects from the webhook benchmark: what broke, why, and what the fix was.
const BUGS: [string, string, string][] = [
  ["Webhooks never reached the database", "Unquoted camelCase SQL identifiers made every insert fail.", "Quoted the identifiers."],
  ["A captured payment ended up FAILED", "Two events for one payment raced and one overwrote the other.", "Row lock inside a transaction. Rerun as 25 repeated races."],
  ["Retries fired 5.5 hours late", "A timezone slip in the retry timestamp.", "UTC timestamps."],
  ["The outbox stopped draining", "With Redis down, enqueue hung with no timeout.", "A bounded timeout."],
  ["Events stuck in RECEIVED forever", "After a database outage the recovery sweep skipped that state.", "The sweep now covers it."],
  ["The same event queued twice", "Two dispatchers could both claim it.", "Claim inside the transaction."],
  ["A crash marked work as IGNORED", "A worker killed mid-job left the wrong state behind.", "Fixed the state transition. Tested with a real SIGKILL."],
  ["BullMQ refused the job", "Job ids containing colons are not accepted.", "Dropped the id scheme."],
];

const VERSUS: [string, string, string][] = [
  ["Real PostgreSQL", "Yes", "No, in-memory repository"],
  ["Real Redis", "Yes", "No"],
  ["Worker crash", "kill -9 on a child process", "Simulated lease expiry"],
  ["HTTP layer", "Fastify inject", "Code review for several cases"],
  ["Concurrent claims", "25 repeated races", "In-memory"],
  ["Database and queue outages", "Exercised", "Simulated"],
  ["Scenarios", "15 of 15 run", "15 of 15 attempted"],
];

// What a team needs from a coding agent, and the part of zerd that answers it.
const NEEDS: [string, string][] = [
  ["I can’t review everything an agent writes.", "A second model reads the real diff, and a verifier tries to refute each finding. A high-severity claim is reproduced in a sandbox or capped at medium."],
  ["“Tests pass” didn’t cover what it changed.", "Checks run on the changed files. A signature guard sends it back if a public parameter disappeared while the tests stayed green."],
  ["It invented a file, or a line number.", "Every path and path:line in the answer is looked up in your tree before you see it."],
  ["Our provider hit its quota mid-task.", "zerd fails over to the next provider and tells you. Claude, GPT, Codex, DeepSeek and local models all work."],
  ["It must not read .env or touch other repos.", "Enforced in code: confined file tools, a shell that refuses outside paths, and an OS sandbox. Not a line in a prompt."],
  ["Every session starts from zero.", "Your corrections become lessons in your own AGENTS.md. You can read them, edit them and commit them."],
  ["The bill grows with the session.", "It remembers what it has read and sends only new lines or a diff. Cache hits were 87% against 76% in the 11-task run."],
];

const PIPE: [string, string][] = [
  ["Edit", "The agent changes your files."],
  ["Check", "Typecheck, lint and tests run on the changed files."],
  ["Review", "A different model reads the actual diff."],
  ["Refute", "A verifier tries to disprove each finding. Survivors are reproduced in a sandbox."],
  ["Look up", "Each cited path and line is checked on disk."],
  ["Report", "The answer says what ran, what was reviewed and what stayed unverified."],
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
          <p className="lead">zerd tests every change against your real database, has a second model review the diff, and tells you what it could not verify. You merge with proof, not a hunch.</p>
          <CopyCmd cmd={INSTALL} />
          <p className="hero-links">
            <Link href="/docs/try-it-in-two-minutes">Try it on a small change</Link>
            <a href="#versus">See it against Claude Code</a>
          </p>
          <ul className="gains">
            <li><b>Review less.</b> A second model and a verifier read every diff first. You only see findings that held up.</li>
            <li><b>Catch it before production.</b> Crashes, races and outages are tested on real Postgres and Redis. One lost update would have left a captured payment marked FAILED.</li>
            <li><b>Keep your options open.</b> Any model, and when one provider’s quota runs out zerd moves to the next.</li>
          </ul>
        </div>
        <RunLog />
        <p className="hero-rail"><span>One native binary, about 11 MB</span><span>macOS · Linux · Windows</span></p>
      </section>

      <div className="after">
      <section className="sec wrap" id="bugs">
        <div className="sec-head">
          <h2>Eight defects a mock would never show you</h2>
          <p>The task: build a webhook system that survives duplicates, races, crashes and outages. zerd ran 15 attack scenarios against a real Postgres and Redis, reproduced each failure, fixed it and ran it again, one commit per fix.</p>
        </div>
        <div className="bugs" role="table" aria-label="Defects found">
          <div className="bug-h" role="row"><span role="columnheader">What went wrong</span><span role="columnheader">Why</span><span role="columnheader">The fix</span></div>
          {BUGS.map(([a, b, c]) => (
            <div className="bug" role="row" key={a}><strong role="cell">{a}</strong><span role="cell">{b}</span><span role="cell" className="fix">{c}</span></div>
          ))}
        </div>
      </section>

      <section className="sec wrap" id="versus">
        <div className="sec-head">
          <h2>Same task, same requirements. Claude Code stayed in the service layer.</h2>
          <p>zerd on MiniMax M3 against Claude Code on Sonnet 5.5, each on its own Git branch. Claude Code found real problems too: an exception path that left events stuck in PROCESSING, and a refund that could land before its capture. Its report was honest that it never touched real infrastructure.</p>
        </div>
        <div className="vs" role="table" aria-label="zerd and Claude Code compared">
          <div className="vs-h" role="row"><span role="columnheader"></span><span role="columnheader">zerd, MiniMax M3</span><span role="columnheader">Claude Code, Sonnet 5.5</span></div>
          {VERSUS.map(([k, z, c]) => (
            <div className="vs-r" role="row" key={k}><span role="rowheader">{k}</span><span role="cell" className="yes">{z}</span><span role="cell">{c}</span></div>
          ))}
        </div>
        <p className="caveat">One project, one set of scenarios, one run each. This shows depth of verification, not a ranking of models. zerd’s own list of open risks is in the log above: tenant isolation, replay auditing, out-of-order payment events.</p>
        <p className="caveat">On an earlier 11-task set, zerd and Claude Code each passed 10. zerd sent 1.01M prompt tokens against 1.31M, but made 113 model calls against 42 and was slower. Claude Code deleted <code>.git</code> and <code>data/</code> when asked to wipe history. zerd’s miss, a dropped parameter, is what the signature guard now catches.</p>
      </section>

      <section className="sec wrap">
        <div className="sec-head">
          <h2>Why it can catch these: it will not call work done until it has evidence</h2>
        </div>
        <ol className="pipe">
          {PIPE.map(([t, d], i) => (
            <li key={t}><span className="pipe-n">{i + 1}</span><h3>{t}</h3><p>{d}</p></li>
          ))}
        </ol>
      </section>

      <section className="sec wrap">
        <div className="sec-head">
          <h2>What a team needs from an agent, and the part that answers it</h2>
        </div>
        <dl className="needs">
          {NEEDS.map(([q, a]) => (
            <div key={q}><dt>{q}</dt><dd>{a}</dd></div>
          ))}
        </dl>
      </section>

      <section className="sec wrap">
        <div className="sec-head">
          <h2>A real run, not a mock-up</h2>
          <p>zerd on MiniMax M3 fixing a retry bug in a small repo, then running its own checks.</p>
        </div>
        <Demo />
      </section>

      <section className="sec wrap install" id="install">
        <div className="sec-head">
          <h2>Try it on a small change</h2>
          <p>One binary, no runtime. First run walks you through a provider, and <code>zerd doctor</code> tells you what is missing.</p>
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
