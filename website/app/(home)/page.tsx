import Link from "next/link";
import Image from "next/image";
import { latestRelease } from "@/lib/latest";
import { JsonLd } from "@/lib/jsonld";
import Hero from "./hero";
import Ledger, { type Row } from "./ledger";
import Install from "./install";
import Lie from "./lie";
import Round from "./round";
import AppWindow from "./window";
import "./sky.css";

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
            <p className="eyebrow">A real session on Tunyt</p>
            <h2><span className="h-sans">No “done!” at the end.</span> <span className="h-serif">A summary that says what was checked.</span></h2>
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
            <p className="lie-lead">Same answer from two agents. Flip the switch and see what zerd does to the file path and to the claim that tests passed.</p>
          </div>
          <div><Lie /></div>
        </div>
      </section>

      <Round />

      <section className="wrap ledger">
        <div>
          <p className="eyebrow">The difference</p>
          <h2><span className="h-sans">What zerd does</span> <span className="h-serif">that most agents don’t.</span></h2>
        </div>
        <Ledger rows={rows} />
        <p className="rule">Every token and every tool call should earn its place.</p>
      </section>


      <section className="wrap two">
        <div>
          <p className="eyebrow">Safety</p>
          <h2><span className="h-sans">Two questions,</span> <span className="h-serif">answered in code.</span></h2>
        </div>
        <div className="two-cols">
          <div>
            <h3>What can it touch?</h3>
            <p>File tools stay inside the folder you started in; <code>..</code> and symlinks that point out are refused. Shell commands are scanned for paths in your home, <code>/etc</code> or other users’ folders before they run. On macOS an OS sandbox backs both up, and <code>.env</code>, keys and SSH folders are protected by default.</p>
          </div>
          <div>
            <h3>When does it ask?</h3>
            <p>Seven approval modes, from <code>plan</code> (read-only) to <code>auto</code>, and Shift+Tab cycles them. Approvals show the diff, and what you approve is exactly what gets written. <code>/undo</code> restores what the last request changed.</p>
          </div>
        </div>
        <p className="two-note">The kernel sandbox is macOS only. On Linux and Windows you get the confined tools and the shell scan, not the OS layer.</p>
      </section>

      <section className="wrap fit">
        <div>
          <p className="eyebrow">Fit</p>
          <h2><span className="h-sans">Use zerd when</span> <span className="h-serif">you want proof.</span></h2>
        </div>
        <div className="fit-cols">
          <ul>
            <li><strong>You review before you merge.</strong> Run a review on the working tree, a commit, a branch or a PR, and get findings a second agent already tried to refute.</li>
            <li><strong>Quota runs out mid-task.</strong> zerd tells you, then moves to the next provider.</li>
            <li><strong>Work splits cleanly.</strong> <code>parallel_tasks</code> gives each worker its own git worktree and merges in dependency order.</li>
          </ul>
          <ul className="not">
            <li>No image input yet.</li>
            <li>No IDE extension.</li>
            <li>The 11-task benchmark came from the Python version, before the Rust port.</li>
            <li>Many defaults are tuned on one repo.</li>
          </ul>
        </div>
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
            <Install big />
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
