import Link from "next/link";
import Image from "next/image";
import { latestRelease } from "@/lib/latest";
import CopyCmd from "./copy-cmd";
import "./home.css";

const INSTALL = "curl -fsSL https://zerd.si/install.sh | sh";

// Each row: what most agents do, then zerd's answer as [plain, bold, plain].
// Wording follows the "what makes it different" docs page.
const rows: [string, [string, string, string]][] = [
  ["Say “done”", ["Run checks on the changed files, then have a ", "different model review the real diff", ""]],
  ["Show every review finding", ["A ", "verifier tries to refute each one", "; high-severity claims are reproduced in a sandbox or capped"]],
  ["Quietly invent a file or line", ["Every path:line in the answer is ", "checked against your tree", ""]],
  ["Re-send the whole file every turn", ["Remember what was read; send only ", "new lines or a diff", ""]],
  ["Ask the prompt to behave (“never read .env”)", ["", "Enforce it in code", ": confined tools, a path-scanning shell, an OS sandbox"]],
  ["Die when a provider’s quota runs out", ["", "Fail over", " to another provider and tell you"]],
  ["Forget what you told them", ["Turn your corrections into ", "lessons in your own AGENTS.md", ""]],
  ["Break an old caller while “refactoring”", ["", "Signature guard", " sends it back if a public parameter disappeared"]],
];

// Receipt lines. Delay in the stylesheet comes from the --i index.
const receipt: [string, string, string][] = [
  ["ok", "Biome check on 3 changed files", "passed"],
  ["ok", "TypeScript check", "passed"],
  ["ok", "Review of the real diff by a second model", "done"],
  ["ok", "Every path:line looked up on disk", "found"],
];

export default async function Home() {
  const rel = await latestRelease();
  return (
    <main className="home">
      <section className="hero">
        <div className="hero-copy">
          <h1>Most agents say done. zerd shows the receipt.</h1>
          <p className="lead">
            zerd is a terminal coding agent. After it edits your code it runs the checks that cover your changed files,
            gets a second model to review the real diff, and looks up every file path it mentions.
          </p>
          <CopyCmd cmd={INSTALL} />
          <p className="win">Windows: <code>irm https://zerd.si/install.ps1 | iex</code></p>
          <div className="actions">
            <Link className="btn" href="/docs/try-it-in-two-minutes">Get started</Link>
            <Link className="plain" href="/docs/what-makes-it-different">Why zerd</Link>
            {rel && <span className="ver">v{rel.version}</span>}
          </div>
        </div>

        <div className="pair">
          <figure className="term muted">
            <figcaption>What most agents say</figcaption>
            <pre>Done! I fixed the bug.</pre>
          </figure>
          <figure className="term receipt">
            <figcaption>What zerd shows</figcaption>
            <pre>
              {receipt.map(([k, text, res], i) => (
                <span className="line" style={{ ["--i" as string]: i }} key={text}>
                  <span className={k}>✓</span> {text}: {res}{"\n"}
                </span>
              ))}
              <span className="line" style={{ ["--i" as string]: receipt.length }}>{"\n"}status: <span className="ok">verified</span></span>
            </pre>
          </figure>
        </div>
      </section>

      <section className="shot">
        <figure>
          <Image src="/docs/assets/screenshot.png" alt="A zerd session in the terminal, ending in a verified summary" width={3024} height={1780} sizes="(min-width: 1200px) 1144px, 100vw" priority />
          <figcaption>A real session, not a mock-up.</figcaption>
        </figure>
      </section>

      <section className="ledger">
        <h2>What zerd does that most agents don’t</h2>
        <div className="head" aria-hidden="true"><span>Most agents</span><span>zerd</span></div>
        {rows.map(([bad, [a, b, c]]) => (
          <div className="row" key={bad}>
            <p className="bad"><span className="who">Most agents </span>{bad}</p>
            <p className="good"><span className="who">zerd </span>{a}<strong>{b}</strong>{c}</p>
          </div>
        ))}
        <p className="rule">Every token and every tool call should earn its place.</p>
      </section>

      <section className="install">
        <h2>Install it and try it on a small change.</h2>
        <p>One binary, no runtime to set up. Paste this in a terminal.</p>
        <CopyCmd cmd={INSTALL} />
        <p className="win">Windows: <code>irm https://zerd.si/install.ps1 | iex</code></p>
        <div className="actions">
          <Link className="btn" href="/docs/try-it-in-two-minutes">Get started</Link>
          <Link className="plain" href="/docs/what-makes-it-different">Why zerd</Link>
        </div>
      </section>

      <p className="models">
        zerd works with Claude, GPT, Codex, DeepSeek and local models, on macOS, Linux and Windows.{" "}
        <Link href="/docs/models-and-routing">How models are chosen and routed</Link>.
      </p>
    </main>
  );
}
