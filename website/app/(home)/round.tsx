// The Tunyt head-to-head as the receipt's subtotal. Figures are from the README's "Adversarial round"
// section and the two worktrees on disk. The caveats are printed as tax and fees, right under the
// numbers, so nobody has to scroll to find out what the numbers don't prove.
const rows = [
  ["scenarios with a test", "15 / 15", "14 / 15"],
  ["bugs found and reproduced", "12", "3"],
  ["database", "Postgres", "in-memory"],
  ["worker crash", "SIGKILL", "simulated"],
  ["HTTP layer", "real app", "service"],
] as const;

export default function Round() {
  return (
    <section className="p-sec" id="round">
      <span className="stamp stamp-red" aria-hidden="true">not a verdict</span>
      <h2>Same brief. Two agents. Then we attacked what each one built.</h2>
      <p className="p-lead">
        The brief: a payment-webhook service for Tunyt. Signature checks, idempotency, an outbox, retries, replay, an admin API.
        Each agent worked in its own git worktree. Then both faced the same failures: 100 identical webhooks at once, a worker killed
        mid-job, Redis down, Postgres down, a crash between commit and ack.
      </p>

      <div className="tab" role="table" aria-label="zerd against Claude Code on the same task">
        <div className="tr th" role="row"><span role="columnheader" /><i /><b role="columnheader">zerd</b><b role="columnheader">Claude Code</b></div>
        {rows.map(([k, a, b]) => (
          <div className="tr" role="row" key={k}><span role="rowheader">{k}</span><i /><b role="cell">{a}</b><b role="cell">{b}</b></div>
        ))}
      </div>

      <div className="two-up">
        <div>
          <h3>zerd found 12</h3>
          <p>Unquoted camelCase SQL that stopped any webhook from being saved. A lost update that left a captured payment marked failed. Retries scheduled about 5.5 hours late on a machine set to IST. It tested against a real Postgres and killed a real worker with SIGKILL.</p>
        </div>
        <div>
          <h3>Claude Code found 3</h3>
          <p>One is a refund applied before its capture when events arrive out of order. zerd missed that one. It tested the service layer with an in-memory repository and simulated the lease expiry.</p>
        </div>
      </div>

      <h3 className="p-h3">Tax and fees</h3>
      <ul className="fees">
        <li><b>Not the same conditions.</b> Claude Code tested the service layer only, so part of the gap is how deep each one tested, not only how well it engineered.</li>
        <li><b>Not production-ready.</b> zerd&rsquo;s own report lists merchant-controlled tenant ids, one shared secret and an open <code>/metrics</code>.</li>
        <li><b>One edit, disclosed.</b> zerd changed one assertion and said so. It also flagged that one of its commit messages overstates what it verified.</li>
        <li><b>One task, one run each.</b> The controlled re-run is still to do.</li>
      </ul>
      <pre className="inset" aria-label="git worktree list output"><span className="d">$ git worktree list</span>{"\n"}tunyt-claude  [benchmark/claude-webhook]{"\n"}tunyt-melon   [benchmark/melon-webhook]  <span className="d"># melon was zerd&rsquo;s old name</span></pre>
    </section>
  );
}
