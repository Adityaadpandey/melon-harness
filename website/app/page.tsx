import Link from "next/link";
import { latestRelease } from "@/lib/latest";

const features = [
  ["Checks, then a second opinion", "Runs the checks that cover your changed files, then a different model reviews the real diff."],
  ["Findings get cross-examined", "A verifier tries to refute each review finding. High-severity claims are reproduced in a sandbox or capped."],
  ["No invented paths", "Every path:line in the answer is looked up in your tree before you see it."],
  ["Cheap by design", "Remembers what it read and sends only new lines or a diff. 85% of input comes from cache in a real session."],
  ["Rules enforced in code", "Confined tools, a path-scanning shell and an OS sandbox, not a prompt asking nicely."],
  ["Fails over, and says so", "When a provider hits its quota zerd switches to another one and tells you."],
];

export default async function Home() {
  const rel = await latestRelease();
  return (
    <main>
      <section className="hero">
        <p className="pill">{rel ? `v${rel.version}` : "open source"} · one native Rust binary</p>
        <h1>The terminal coding agent that proves its work.</h1>
        <p className="lead">
          Ask most agents to fix a bug and you get “Done!”. Ask zerd and you get what actually happened: the checks that ran, a
          second model’s review of the real diff, and every file path looked up on disk.
        </p>
        <pre className="cmd">curl -fsSL https://zerd.si/install.sh | sh</pre>
        <p className="small">
          Windows: <code>irm https://zerd.si/install.ps1 | iex</code>
        </p>
        <div className="actions">
          <Link className="btn primary" href="/docs/try-it-in-two-minutes">Get started</Link>
          <Link className="btn" href="/docs/what-makes-it-different">Why zerd</Link>
        </div>
      </section>

      <section className="shot">
        <img src="/docs/assets/screenshot.png" alt="A real zerd session: side panel, token breakdown, verification and review" />
        <p className="small">A real session, not a mock-up.</p>
      </section>

      <section className="grid">
        {features.map(([t, d]) => (
          <div className="card" key={t}>
            <h3>{t}</h3>
            <p>{d}</p>
          </div>
        ))}
      </section>

      <section className="hero">
        <h2>Works with the models you already use</h2>
        <p className="lead">Claude, GPT, Codex, DeepSeek and local models. macOS, Linux and Windows.</p>
        <Link className="btn" href="/docs/models-and-routing">Models and routing</Link>
      </section>
    </main>
  );
}
