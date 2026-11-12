import Link from "next/link";
import Image from "next/image";
import { latestRelease } from "@/lib/latest";
import { CloudLayer, type Placed } from "./clouds";
import CopyCmd from "./copy-cmd";
import Hero from "./hero";
import Ledger, { type Row } from "./ledger";
import Lie from "./lie";
import Perf from "./perf";
import Reveal from "./reveal";
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

const SHOT_CLOUDS: Placed[] = [
  { shape: "b", left: "-12%", top: 40, width: "44vw", speed: 0.12, drift: 200, delay: 60, tone: 0.9 },
  { shape: "d", left: "62%", top: 380, width: "50vw", speed: 0.16, drift: 240, delay: 120, tone: 0.85 },
];
const END_CLOUDS: Placed[] = [
  { shape: "c", left: "-10%", top: 20, width: "60vw", speed: 0.1, drift: 210, delay: 30 },
  { shape: "a", left: "48%", top: 120, width: "66vw", speed: 0.14, drift: 260, delay: 140 },
  { shape: "b", left: "10%", top: 330, width: "70vw", speed: 0.2, drift: 180, delay: 80, z: 2 },
];

export default async function Home() {
  const rel = await latestRelease();
  return (
    <main className="sky">
      <Hero />

      <section className="shot">
        <CloudLayer clouds={SHOT_CLOUDS} />
        <div className="wrap shot-grid">
          <Reveal className="shot-text">
            <h2><span className="h-sans">Not a replay this time.</span> <span className="h-serif">A real session, ending in a verified summary.</span></h2>
          </Reveal>
          <Reveal className="shot-win" delay={0.1}>
            <AppWindow title="zerd · ~/Devlopment/tunyt">
              <Image src="/docs/assets/screenshot.png" alt="A zerd session in the terminal, ending in a verified summary" width={3024} height={1780} sizes="(min-width: 1200px) 1100px, 100vw" />
            </AppWindow>
          </Reveal>
        </div>
      </section>

      <section className="lie-sec">
        <div className="wrap lie-grid">
          <Reveal>
            <h2><span className="h-sans">Try to make</span> <span className="h-serif">an agent lie.</span></h2>
            <p className="lie-lead">Same answer, two agents. Flip the switch and watch what the lookup does to the path and the test claim.</p>
          </Reveal>
          <Reveal delay={0.1}><Lie /></Reveal>
        </div>
      </section>

      <Perf />

      <section className="wrap ledger">
        <Reveal>
          <h2><span className="h-sans">What zerd does</span> <span className="h-serif">that most agents don’t.</span></h2>
        </Reveal>
        <Ledger rows={rows} />
        <p className="rule">Every token and every tool call should earn its place.</p>
      </section>

      <Perf />

      <section className="end" id="install">
        <CloudLayer clouds={END_CLOUDS} />
        <div className="wrap end-in">
          <Reveal>
            <h2><span className="h-sans">Try it on</span> <span className="h-serif">a small change.</span></h2>
            <p className="end-lead">One binary, no runtime to set up. Paste this in a terminal.</p>
            <CopyCmd cmd={INSTALL} big />
            <p className="win-cmd">Windows: <code>irm https://zerd.si/install.ps1 | iex</code></p>
            <div className="hero-cta">
              <Link className="btn-dark" href="/docs/try-it-in-two-minutes"><span>Get started</span><b aria-hidden="true">→</b></Link>
              <Link className="btn-glass" href="/docs/what-makes-it-different">Why zerd</Link>
              {rel && <span className="ver">v{rel.version}</span>}
            </div>
          </Reveal>
        </div>
      </section>
    </main>
  );
}
