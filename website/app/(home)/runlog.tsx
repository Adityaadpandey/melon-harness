// The hero's right side: the defects the webhook benchmark turned up, each one reproduced against real
// Postgres and Redis, fixed, and run again. Pure CSS: every row shows FAIL for a moment, then flips to
// PASS once (see .rl-row in landing.css). With reduced motion, or no CSS animation, it just shows PASS.
const ROWS: [string, string][] = [
  ["Save a webhook to Postgres", "unquoted camelCase columns, the insert never ran"],
  ["Two events for one payment, at once", "lost update left a captured payment FAILED"],
  ["Schedule the next retry", "timezone bug, retries landed 5.5 hours late"],
  ["Redis down while enqueueing", "enqueue hung forever, the outbox never drained"],
  ["Postgres down, then back", "events stuck in RECEIVED, the sweep skipped them"],
  ["Two dispatchers, one event", "the same event was enqueued twice"],
  ["kill -9 a worker mid-job", "event wrongly marked IGNORED"],
  ["Queue the job in BullMQ", "job ids with colons were rejected"],
];

export default function RunLog() {
  return (
    <figure className="rl" aria-label="Eight defects zerd reproduced against real Postgres and Redis, then fixed">
      <figcaption className="rl-head">
        <span>zerd, webhook reliability audit</span>
        <span className="rl-env">real Postgres · real Redis · SIGKILL</span>
      </figcaption>
      <ol className="rl-body">
        {ROWS.map(([what], i) => (
          <li key={what} className="rl-row" style={{ "--i": i } as React.CSSProperties}>
            <span className="rl-badge" aria-hidden="true"><b className="bad">FAIL</b><b className="ok">PASS</b></span>
            <span className="rl-what">{what}</span>
            <span className="sr-only">: fixed, rerun passes</span>
          </li>
        ))}
      </ol>
      <p className="rl-foot">8 defects reproduced, fixed and rerun. Still open: out-of-order payments, tenant isolation.</p>
    </figure>
  );
}
