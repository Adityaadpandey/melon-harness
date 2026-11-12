import Link from "next/link";
import Image from "next/image";
import { latestRelease } from "@/lib/latest";
import CopyCmd from "./copy-cmd";
import Demo from "./demo";
import Lie from "./lie";
import "./home.css";

const INSTALL = "curl -fsSL https://zerd.si/install.sh | sh";

// Each row: what most agents do, zerd's answer as [plain, bold, plain], and one line on how
// it is enforced. Wording follows the "what makes it different" page and the docs it links to.
const rows: [string, [string, string, string], string][] = [
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

export default async function Home() {
  const rel = await latestRelease();
  return (
    <main className="home">
      <section className="band-dark">
        <div className="wrap">
          <div className="hero-top">
            <h1>Most agents say done. zerd shows the receipt.</h1>
            <div className="hero-side">
              <p className="lead">
                zerd is a terminal coding agent. After it edits your code it runs the checks that cover your changed files,
                gets a second model to review the real diff, and looks up every file path it mentions.
              </p>
              <CopyCmd cmd={INSTALL} />
            </div>
          </div>
          <Demo />
        </div>
      </section>

      <section className="wrap shot">
        <figure>
          <div className="frame">
            <Image src="/docs/assets/screenshot.png" alt="A zerd session in the terminal, ending in a verified summary" width={3024} height={1780} sizes="(min-width: 1200px) 1100px, 100vw" />
          </div>
          <figcaption><b>Not a replay this time.</b> A real session, ending in a verified summary.</figcaption>
        </figure>
      </section>

      <section className="band-soft">
        <div className="wrap lie-grid">
          <div>
            <h2>Try to make an agent lie.</h2>
            <p>Same answer, two agents. Flip the switch and watch what the lookup does to the path and the test claim.</p>
          </div>
          <Lie />
        </div>
      </section>

      <section className="wrap ledger">
        <h2>What zerd does that most agents don’t</h2>
        <div className="head" aria-hidden="true"><span>Most agents</span><span>zerd</span></div>
        {rows.map(([bad, [a, b, c], how]) => (
          <details className="row" key={bad}>
            <summary>
              <p className="bad"><span className="who">Most agents </span>{bad}</p>
              <p className="good"><span className="who">zerd </span>{a}<strong>{b}</strong>{c}</p>
              <span className="plus" aria-hidden="true" />
            </summary>
            <p className="how">{how}</p>
          </details>
        ))}
        <p className="rule">Every token and every tool call should earn its place.</p>
      </section>

      <section className="band-cobalt">
        <div className="wrap install">
          <h2>Install it and try it on a small change.</h2>
          <p>One binary, no runtime to set up. Paste this in a terminal.</p>
          <CopyCmd cmd={INSTALL} big />
          <p className="win">Windows: <code>irm https://zerd.si/install.ps1 | iex</code></p>
          <div className="actions">
            <Link className="btn" href="/docs/try-it-in-two-minutes">Get started</Link>
            <Link className="plain" href="/docs/what-makes-it-different">Why zerd</Link>
            {rel && <span className="ver">v{rel.version}</span>}
          </div>
        </div>
      </section>

      <p className="wrap models">
        zerd works with Claude, GPT, Codex, DeepSeek and local models, on macOS, Linux and Windows.{" "}
        <Link href="/docs/models-and-routing">How models are chosen and routed</Link>.
      </p>
    </main>
  );
}
