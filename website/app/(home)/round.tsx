// The long version of the receipt in the hero: what was run, what each agent found, and why
// the comparison is not a verdict. Numbers come from the README's "Adversarial round" section
// and the two worktrees on disk.
export default function Round() {
  return (
    <section className="z-sec" id="round">
      <div className="z-wrap z-round">
        <div>
          <h2>We gave zerd and Claude Code the same brief, then attacked what each one built.</h2>
          <p className="z-lead">
            The brief: build a payment-webhook service for Tunyt. Signature checks, idempotency, an outbox, retries, replay and an admin API.
            Each agent worked in its own git worktree. Then we hit both with the same failures: 100 identical webhooks at once,
            a worker killed mid-job, Redis down, Postgres down, a crash between commit and ack.
          </p>
        </div>

        <div className="z-round-cols">
          <div className="z-card">
            <h3>zerd found 12 bugs</h3>
            <p>Unquoted camelCase SQL that stopped any webhook from being saved. A lost update that left a captured payment marked failed. Retries scheduled about 5.5 hours late on a machine set to IST.</p>
            <p>It tested against a real Postgres and killed a real worker process with SIGKILL.</p>
          </div>
          <div className="z-card">
            <h3>Claude Code found 3</h3>
            <p>One of them is a refund applied before its capture when events arrive out of order. zerd missed that.</p>
            <p>It tested the service layer with an in-memory repository and simulated the lease expiry.</p>
          </div>
        </div>

        <div className="z-fair">
          <h3>Why this is not a win</h3>
          <ul>
            <li>The two runs were not under the same conditions. Claude Code tested the service layer only, so part of the gap is how deep each one tested, not only how well it engineered.</li>
            <li>Neither result is production-ready. zerd&rsquo;s own report lists merchant-controlled tenant ids, one shared secret and an open <code>/metrics</code>.</li>
            <li>zerd changed one assertion and said so. It also flagged that one of its commit messages overstates what it verified.</li>
            <li>It was one task and one run each. The controlled re-run is still to do.</li>
          </ul>
          <pre className="z-term" aria-label="git worktree list output"><span className="d">$ git worktree list</span>{"\n"}tunyt-claude  [benchmark/claude-webhook]{"\n"}tunyt-melon   [benchmark/melon-webhook]  <span className="d"># melon was zerd&rsquo;s old name</span></pre>
        </div>
      </div>
    </section>
  );
}
