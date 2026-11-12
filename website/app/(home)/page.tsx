import Link from "next/link";
import Image from "next/image";
import { latestRelease } from "@/lib/latest";
import { JsonLd } from "@/lib/jsonld";
import CopyCmd from "./copy-cmd";
import Hero from "./hero";
import Ledger, { type Row } from "./ledger";
import Lie from "./lie";
import AppWindow from "./window";
import "./sky.css";

const INSTALL = "curl -fsSL https://zerd.si/install.sh | sh";

// Each row: what most agents do, zerd's answer as [plain, bold, plain], and one line on how
// it is enforced. Wording follows the "what makes it different" page and the docs it links to.
const rows: Row[] = [
  ["Say “done”", ["Run checks on the changed files, then have a ", "different model review the real diff", ""],
    "The answer states what was reviewed, how many commits, and every fallback taken."],
  ["Show every review finding", ["A ", "verifier tries to refute each one", "; high-severity claims are reproduced in a sandbox or capped"],
    "A claim the verifier refuted, or you called wrong, is remembered per repo for two weeks and shown to the next review."],
  ["Quietly invent a file or line", ["Every path:line in the answer is ", "checked against your tree", ""],
    "An always-on rule says to only claim what a tool showed you."],
  ["Re-send the whole file every turn", ["Remember what was read; send only ", "new lines or a diff", ""],
    "Incremental reads: only new lines, or a diff since the last copy the model saw."],
  ["Ask the prompt to behave (“never read .env”)", ["", "Enforce it in code", ": confined tools, a path-scanning shell, an OS sandbox"],
    "File tools resolve the path first, a shell scan refuses paths outside the workspace before running, and the OS sandbox (macOS sandbox-exec, on by default) backs both up."],
  ["Die when a provider’s quota runs out", ["", "Fail over", " to another provider and tell you"],
    "On a plan-wide quota error the failing provider is blocked for the session and the next one is tried; your own model is the last resort."],
  ["Forget what you told them", ["Turn your corrections into ", "lessons in your own AGENTS.md", ""],
    "Lessons are written under “Learned by zerd”, so you can review and commit them. /forget N removes one."],
  ["Break an old caller while “refactoring”", ["", "Signature guard", " sends it back if a public parameter disappeared"],
    "When checks pass but the diff drops a parameter, reorders positional ones or removes an export, the agent is sent back once to keep old calls working or say why not."],
];

// Plain question-and-answer pairs, mirrored as FAQ schema so answer engines can quote them.
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
    <main className="sky">
      <JsonLd data={{ "@context": "https://schema.org", "@type": "FAQPage", mainEntity: FAQ.map(([q, a]) => ({ "@type": "Question", name: q, acceptedAnswer: { "@type": "Answer", text: a } })) }} />
      <Hero />

      <section className="shot">
        <div className="wrap shot-grid">
          <div className="shot-text">
            <p className="eyebrow">A real session</p>
            <h2><span className="h-sans">Not a replay this time.</span> <span className="h-serif">A real session, ending in a verified summary.</span></h2>
          </div>
          <div className="shot-win">
            <AppWindow title="zerd · ~/Devlopment/tunyt">
              <Image src="/docs/assets/screenshot.png" alt="A zerd session in the terminal, ending in a verified summary" width={3024} height={1780} sizes="(min-width: 1200px) 1100px, 100vw" />
            </AppWindow>
          </div>
        </div>
      </section>

      <section className="lie-sec">
        <div className="wrap lie-grid">
          <div>
            <p className="eyebrow">Try it</p>
            <h2><span className="h-sans">Try to make</span> <span className="h-serif">an agent lie.</span></h2>
            <p className="lie-lead">Same answer, two agents. Flip the switch and watch what the lookup does to the path and the test claim.</p>
          </div>
          <div><Lie /></div>
        </div>
      </section>

      <section className="wrap ledger">
        <div>
          <p className="eyebrow">The difference</p>
          <h2><span className="h-sans">What zerd does</span> <span className="h-serif">that most agents don’t.</span></h2>
        </div>
        <Ledger rows={rows} />
        <p className="rule">Every token and every tool call should earn its place.</p>
      </section>

      <section className="end" id="install">
        {/* the hero artwork again, cropped to its flower field, so the page ends where it began */}
        <div className="end-bg" aria-hidden="true">
          <picture>
            <source srcSet="/hero/hero.avif" type="image/avif" />
            <img src="/hero/hero.webp" alt="" width={1672} height={941} loading="lazy" decoding="async" />
          </picture>
        </div>
        <div className="wrap end-in">
          <div>
            <p className="eyebrow">Install</p>
            <h2><span className="h-sans">Try it on</span> <span className="h-serif">a small change.</span></h2>
            <p className="end-lead">One binary, no runtime to set up. Paste this in a terminal.</p>
            <CopyCmd cmd={INSTALL} big />
            <p className="win-cmd">Windows: <code>irm https://zerd.si/install.ps1 | iex</code></p>
            <div className="end-cta">
              <Link className="p-btn" href="/docs/try-it-in-two-minutes">Get started</Link>
              <Link className="p-ghost" href="/docs/what-makes-it-different">Why zerd</Link>
              {rel && <span className="ver">v{rel.version}</span>}
            </div>
          </div>
        </div>
      </section>
    </main>
  );
}
