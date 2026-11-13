import Link from "next/link";
import { latestRelease } from "@/lib/latest";
import { JsonLd } from "@/lib/jsonld";
import Hero from "./hero";
import Barcode from "./barcode";
import Install from "./install";
import Lie from "./lie";
import Round from "./round";
import Logo from "./logo";
import Demo from "./demo";
import "./z.css";

// Each line item: what zerd does, what most agents do instead, and how it is enforced.
const rows = [
  ["Runs the checks on what it changed", "Say “done”.", "Then a different model reads the real diff. The answer says what was reviewed, how many commits, and every fallback taken."],
  ["Tries to refute its own review findings", "Show every finding.", "High-severity ones are reproduced in a sandbox or capped. A finding the verifier refuted, or you called wrong, is remembered for two weeks and shown to the next review."],
  ["Looks up every path:line it cites", "Invent a file or a line number.", "An always-on rule says to claim only what a tool showed."],
  ["Sends only what changed in a file", "Re-send the whole file every turn.", "New lines, or a diff since the last copy the model saw."],
  ["Enforces rules in code, not the prompt", "Ask the prompt to behave.", "File tools resolve the path first, a shell scan refuses outside paths before running, and sandbox-exec backs both up on macOS."],
  ["Fails over when a quota runs out", "Stop.", "The failing provider is blocked for the session and the next one is tried. Your own model is the last resort."],
  ["Keeps your corrections", "Forget them.", "They become lessons in your own AGENTS.md, under “Learned by zerd”. You can review and commit them. /forget N removes one."],
  ["Sends back a change that breaks old callers", "Break an old caller while refactoring.", "If checks pass but the diff drops a parameter, reorders positional ones or removes an export, the agent gets one chance to fix it or say why not."],
] as const;

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
    <main className="z-main">
      <JsonLd data={{ "@context": "https://schema.org", "@type": "FAQPage", mainEntity: FAQ.map(([q, a]) => ({ "@type": "Question", name: q, acceptedAnswer: { "@type": "Answer", text: a } })) }} />
      <Hero />

      <div className="sheet">
        <div className="sheet-in">
          <header className="p-head">
            <Logo />
            <p>terminal coding agent</p>
            <p>zerd.si{rel ? ` · v${rel.version}` : ""}</p>
          </header>

          <section className="p-sec">
            <h2>Every &ldquo;done&rdquo; is a claim.</h2>
            <p className="p-lead">A coding agent talks in claims. Fixed. Tests pass. The bug was on line 88. You can&rsquo;t tell which are true without redoing the work, and that was the work you handed over.</p>
            <p className="p-lead">zerd checks each claim before you see it: tests it actually ran, a diff a second model actually read, paths that actually exist. Whatever it couldn&rsquo;t check, it says so.</p>
            <p className="p-hint">Same answer, before and after. Flip it.</p>
            <Lie />
          </section>

          <Round />

          <section className="p-sec">
            <h2>A real run.</h2>
            <p className="p-lead">The binary on a small repo, recorded as it happened. Watch the end: it ran its own checks before it called the fix done.</p>
            <Demo />
          </section>

          <section className="p-sec">
            <h2>What you get on the bill.</h2>
            <ul className="items">
              {rows.map(([item, bad, how]) => (
                <li key={item}>
                  <div className="it"><span>{item}</span><i /><b aria-hidden="true">✓</b></div>
                  <p className="it-bad"><span className="sr-only">Most agents: </span>most agents: {bad.toLowerCase()}</p>
                  <p className="it-how">{how}</p>
                </li>
              ))}
            </ul>
          </section>

          <section className="p-sec">
            <span className="stamp stamp-red" aria-hidden="true">not included</span>
            <h2>What it doesn&rsquo;t do yet.</h2>
            <ul className="no">
              <li><b>Image input.</b> Text and code only.</li>
              <li><b>An IDE extension.</b> It is a terminal program.</li>
              <li><b>The OS sandbox on Linux or Windows.</b> You still get the confined tools and the shell scan.</li>
              <li><b>A benchmark for this binary.</b> The 11-task run (10 of 11 each, with different models) was done on the Python version, before the Rust port.</li>
              <li><b>Defaults tuned beyond one repo.</b> Yours may want different ones.</li>
            </ul>
            <h3 className="p-h3">Small print</h3>
            <p className="p-lead">File tools stay inside the folder you started in. <code>..</code> and symlinks that point out are refused. Shell commands are scanned for paths in your home, <code>/etc</code> or other users&rsquo; folders before they run. On macOS an OS sandbox backs that up, and <code>.env</code>, keys and SSH folders are protected by default. There are seven approval modes, from <code>plan</code> (read-only) to <code>auto</code>. Approvals show the diff, and what you approve is what gets written. <code>/undo</code> restores what the last request changed.</p>
          </section>

          <section className="p-sec p-total" id="install">
            <div className="total-row"><span>TOTAL DUE</span><i /><b>1 command</b></div>
            <Install big />
            <p className="p-hint">One binary. Nothing else to install. Open a repo and ask for a bug fix.</p>
            <Barcode text="curl -fsSL https://zerd.si/install.sh | sh" />
            <p className="p-thanks">Thank you. Nothing on this receipt was claimed that wasn&rsquo;t checked.</p>
            <p className="p-links"><Link href="/docs/try-it-in-two-minutes">Two-minute start</Link> · <Link href="/docs/what-makes-it-different">What makes it different</Link></p>
          </section>
        </div>
      </div>
    </main>
  );
}
