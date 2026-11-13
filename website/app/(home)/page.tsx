import Link from "next/link";
import { latestRelease } from "@/lib/latest";
import { JsonLd } from "@/lib/jsonld";
import Hero from "./hero";
import Install from "./install";
import Lie from "./lie";
import Round from "./round";
import Demo from "./demo";
import "./z.css";

// What most agents do, what zerd does instead, and where it is enforced.
const rows = [
  ["Say “done”.", "Runs the checks on the changed files, then has a different model read the real diff.", "The answer says what was reviewed, how many commits, and every fallback taken."],
  ["Show every review finding.", "A verifier tries to refute each finding first. High-severity ones are reproduced in a sandbox or capped.", "A finding the verifier refuted, or you called wrong, is remembered for two weeks and shown to the next review."],
  ["Invent a file or a line number.", "Looks up every path:line in the answer against your tree.", "An always-on rule says to claim only what a tool showed."],
  ["Re-send the whole file every turn.", "Sends only the new lines, or a diff since the last copy the model saw.", ""],
  ["Ask the prompt to behave.", "Enforces it in code: confined file tools, a shell path scan, and an OS sandbox on macOS.", "File tools resolve the path first, the shell scan refuses outside paths before running, and sandbox-exec backs both up."],
  ["Stop when the quota runs out.", "Fails over to the next provider and tells you.", "The failing provider is blocked for the session. Your own model is the last resort."],
  ["Forget your corrections.", "Writes them as lessons in your own AGENTS.md.", "They sit under “Learned by zerd” so you can review and commit them. /forget N removes one."],
  ["Break an old caller while refactoring.", "Sends the change back if a public parameter disappeared.", "If checks pass but the diff drops a parameter, reorders positional ones or removes an export, the agent gets one chance to fix it or say why not."],
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
      <section className="z-sec">
        <div className="z-wrap z-split">
          <div>
            <h2>Every &ldquo;done&rdquo; is a claim.</h2>
            <p className="z-lead">A coding agent talks in claims. Fixed. Tests pass. The bug was on line 88. You can&rsquo;t tell which are true without redoing the work, and that was the work you handed over.</p>
            <p className="z-lead">zerd checks each claim before you see it: tests it actually ran, a diff a second model actually read, paths that actually exist. Whatever it couldn&rsquo;t check, it says so.</p>
            <p className="z-hint">Flip the switch to see the same answer before and after.</p>
          </div>
          <Lie />
        </div>
      </section>

      <Round />

      <section className="z-sec">
        <div className="z-wrap">
          <h2>A real run.</h2>
          <p className="z-lead">This is the binary on a small repo, recorded as it happened. Watch the end: it ran its own checks before it called the fix done.</p>
          <Demo />
        </div>
      </section>


      <section className="z-sec">
        <div className="z-wrap">
          <h2>Eight things it does that most agents leave to the prompt.</h2>
          <div className="z-rows-head" aria-hidden="true"><span>Most agents</span><span>zerd</span></div>
          <dl className="z-rows">
            {rows.map(([bad, good, how]) => (
              <div className="z-row" key={bad}>
                <dt><span className="sr-only">Most agents: </span>{bad}</dt>
                <dd><span className="sr-only">zerd: </span>{good}{how && <small>{how}</small>}</dd>
              </div>
            ))}
          </dl>
        </div>
      </section>

      <section className="z-sec">
        <div className="z-wrap z-split">
          <div>
            <h2>What it can touch, and when it asks.</h2>
            <p className="z-lead">File tools stay inside the folder you started in. <code>..</code> and symlinks that point out are refused. Shell commands are scanned for paths in your home, <code>/etc</code> or other users&rsquo; folders before they run. On macOS an OS sandbox backs that up, and <code>.env</code>, keys and SSH folders are protected by default.</p>
            <p className="z-lead">There are seven approval modes, from <code>plan</code> (read-only) to <code>auto</code>. Shift+Tab cycles them. Approvals show the diff, and what you approve is what gets written. <code>/undo</code> restores what the last request changed.</p>
          </div>
          <div>
            <h3 className="z-h3">What it doesn&rsquo;t do yet</h3>
            <ul className="z-no">
              <li><b>No image input.</b> Text and code only.</li>
              <li><b>No IDE extension.</b> It is a terminal program.</li>
              <li><b>No OS sandbox on Linux or Windows.</b> You still get the confined tools and the shell scan.</li>
              <li><b>No benchmark for this binary.</b> The 11-task run (10 of 11 each, with different models) was done on the Python version, before the Rust port.</li>
              <li><b>Defaults tuned on one repo.</b> Yours may want different ones.</li>
            </ul>
          </div>
        </div>
      </section>

      <section className="z-sec z-end" id="install">
        <div className="z-wrap">
          <h2>Try it on a small change.</h2>
          <p className="z-lead">One binary, nothing else to install. Paste this in a terminal, then run <code>zerd</code> in a repo: the first launch connects a model in about a minute, and <code>zerd doctor</code> checks the setup any time.</p>
          <Install big />
          <p className="z-meta">
            <Link href="/docs/try-it-in-two-minutes">Two-minute start</Link> · <Link href="/docs/what-makes-it-different">What makes it different</Link>{rel ? ` · v${rel.version}` : ""}
          </p>
        </div>
      </section>
    </main>
  );
}
