// The head-to-head, written the way the README tells it: what was run, what came out, and what
// makes the comparison unfair. Every number here is from README "Adversarial round" and the repo on disk.
const rows = [
  ["Scenarios with an automated test", "15 of 15", "14 of 15"],
  ["Database", "Real PostgreSQL (schema-only copy)", "In-memory repository"],
  ["Worker crash", "Child process killed with SIGKILL", "Lease expiry simulated"],
  ["HTTP", "The real Fastify app", "Service layer only"],
  ["Bugs found and reproduced", "12", "3"],
] as const;

export default function Round() {
  return (
    <section className="round">
      <div className="wrap">
        <p className="eyebrow">The test</p>
        <h2><span className="h-sans">Same brief, two worktrees.</span> <span className="h-serif">Then we attacked what each one built.</span></h2>
        <p className="round-lead">
          We gave zerd and Claude Code the same prompt: build a payment-webhook service for Tunyt, with signatures, idempotency,
          an outbox, retries, replay and an admin API. Each agent got its own git worktree and branch. Then both faced the same
          failures: 100 identical webhooks at once, a worker killed mid-job, Redis down, Postgres down, a crash between commit and ack.
        </p>

        <div className="round-grid">
          <div className="win round-term">
            <div className="win-bar"><span className="win-lights" aria-hidden="true"><i /><i /><i /></span><span className="win-title">git worktree list</span></div>
            <pre>
<span className="t-dim">$ git worktree list</span>{"\n"}
benchmark-melon-claude  <span className="t-blue">[main]</span>{"\n"}
tunyt-claude            <span className="t-blue">[benchmark/claude-webhook]</span>{"\n"}
tunyt-melon             <span className="t-blue">[benchmark/melon-webhook]</span>{"\n\n"}
<span className="t-dim"># melon was zerd’s name when this ran</span>
            </pre>
          </div>

          <table className="round-tab">
            <thead><tr><th scope="col"><span className="sr-only">What was checked</span></th><th scope="col">zerd</th><th scope="col">Claude Code</th></tr></thead>
            <tbody>
              {rows.map(([k, a, b]) => (<tr key={k}><th scope="row">{k}</th><td>{a}</td><td>{b}</td></tr>))}
            </tbody>
          </table>
        </div>

        <div className="round-found">
          <p><strong>zerd found</strong> unquoted camelCase SQL that stopped any webhook from persisting, a lost update that left a captured payment marked failed, and retries scheduled about 5.5 hours late on an IST machine.</p>
          <p><strong>Claude Code found</strong> a refund applying before its capture when events arrive out of order. zerd missed that one.</p>
        </div>

        <div className="round-fair">
          <h3>Why this is not a win</h3>
          <ul>
            <li>The runs were not under the same conditions. Claude Code tested the service layer only, so this compares depth of testing as much as engineering.</li>
            <li>Neither result is ready for production. zerd’s own report lists merchant-controlled tenant ids, one shared secret and an open <code>/metrics</code>.</li>
            <li>zerd changed one assertion and said so. It also flagged that one of its commit messages overstates what it verified.</li>
            <li>No winner is declared. The controlled re-run is still to do.</li>
          </ul>
        </div>
      </div>
    </section>
  );
}
