import Link from "next/link";
import { latestRelease } from "@/lib/latest";
import CopyCmd from "./copy-cmd";
import "./home.css";

const features = [
  ["Checks, then a second opinion", "Runs the checks that cover your changed files, then a different model reviews the real diff."],
  ["Findings get cross-examined", "A verifier tries to refute each review finding. High-severity claims are reproduced in a sandbox or capped."],
  ["No invented paths", "Every path:line in the answer is looked up in your tree before you see it."],
  ["Cheap by design", "Remembers what it read and sends only new lines or a diff. 85% of input comes from cache in a real session."],
  ["Rules enforced in code", "Confined tools, a path-scanning shell and an OS sandbox, not a prompt asking nicely."],
  ["Fails over, and says so", "When a provider hits its quota zerd switches to another one and tells you."],
];

const steps = [
  ["Checks", "Runs the checks that cover the files you changed."],
  ["Second-model review", "A different model reviews the real diff, not a summary of it."],
  ["Verifier and paths", "A verifier tries to refute each finding, and every path:line is looked up on disk."],
];

const models = ["Claude", "GPT", "Codex", "DeepSeek", "Local models"];
const platforms = ["macOS", "Linux", "Windows"];

export default async function Home() {
  const rel = await latestRelease();
  return (
    <main className="home">
      <section className="h-hero">
        <div className="h-tags">
          <span className="tag tag-live">{rel ? `v${rel.version}` : "open source"}</span>
          <span className="tag">One native Rust binary</span>
          <span className="tag">macOS · Linux · Windows</span>
        </div>
        <h1>
          The terminal coding agent
          <br />
          <em>that proves its work.</em>
        </h1>
        <p className="h-lead">
          Ask most agents to fix a bug and you get “Done!”. Ask zerd and you get what actually happened: the checks that ran, a
          second model’s review of the real diff, and every file path looked up on disk.
        </p>
        <CopyCmd cmd="curl -fsSL https://zerd.si/install.sh | sh" />
        <p className="h-win">
          Windows: <code>irm https://zerd.si/install.ps1 | iex</code>
        </p>
        <div className="h-actions">
          <Link className="h-btn primary" href="/docs/try-it-in-two-minutes">Get started</Link>
          <Link className="h-btn" href="/docs/what-makes-it-different">Why zerd</Link>
        </div>
      </section>

      <section className="h-shot">
        <div className="frame">
          <div className="frame-bar">
            <i /><i /><i />
            <span>zerd</span>
          </div>
          <img src="/docs/assets/screenshot.png" alt="A real zerd session: side panel, token breakdown, verification and review" />
        </div>
        <p className="h-cap">A real session, not a mock-up.</p>
      </section>

      <section className="h-sec">
        <p className="label">What you get</p>
        <h2>Evidence, not <em>vibes.</em></h2>
        <div className="h-grid">
          {features.map(([t, d], i) => (
            <div className="h-card" key={t}>
              <span className="num">{String(i + 1).padStart(2, "0")}</span>
              <h3>{t}</h3>
              <p>{d}</p>
            </div>
          ))}
        </div>
      </section>

      <section className="h-sec">
        <p className="label">How it works</p>
        <h2>Three steps before you <em>see “done”.</em></h2>
        <ol className="h-steps">
          {steps.map(([t, d], i) => (
            <li key={t}>
              <span className="num">Step {i + 1}</span>
              <h3>{t}</h3>
              <p>{d}</p>
            </li>
          ))}
        </ol>
      </section>

      <section className="h-sec">
        <p className="label">Models and platforms</p>
        <h2>Works with the models <em>you already use.</em></h2>
        <div className="h-strip">
          {models.map((m) => <span key={m}>{m}</span>)}
          <b />
          {platforms.map((p) => <span key={p}>{p}</span>)}
        </div>
        <Link className="h-link" href="/docs/models-and-routing">Models and routing →</Link>
      </section>

      <section className="h-cta">
        <h2>
          Try it on your next bug.
          <br />
          <em>Read what actually happened.</em>
        </h2>
        <CopyCmd cmd="curl -fsSL https://zerd.si/install.sh | sh" />
        <div className="h-actions">
          <Link className="h-btn primary" href="/docs/try-it-in-two-minutes">Get started</Link>
        </div>
      </section>
    </main>
  );
}
