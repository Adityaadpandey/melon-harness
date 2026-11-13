// The hero object: the Tunyt head-to-head as a printed receipt. It prints once on load, top to
// bottom, in a few stepped jumps like a thermal printer. Every figure is from the README's
// "Adversarial round" section; the caveats are written out in the section below.
const rows = [
  ["scenarios tested", "15 / 15", "14 / 15"],
  ["bugs found and reproduced", "12", "3"],
  ["database", "Postgres", "in-memory"],
  ["worker crash", "SIGKILL", "simulated"],
  ["HTTP layer", "real app", "service"],
] as const;

export default function Receipt() {
  return (
    <div className="rcpt" role="img" aria-label="Receipt comparing zerd and Claude Code on the same payment-webhook task: 15 of 15 scenarios tested against 14 of 15, 12 bugs found against 3.">
      <div className="rcpt-paper" aria-hidden="true">
        <p className="rcpt-title">SAME BRIEF, TWO AGENTS</p>
        <p className="rcpt-sub">payment-webhook service for Tunyt</p>
        <hr />
        <div className="rr rr-head"><span /><i /><b>zerd</b><b>Claude Code</b></div>
        {rows.map(([k, a, b]) => (
          <div className="rr" key={k}><span>{k}</span><i /><b>{a}</b><b>{b}</b></div>
        ))}
        <hr />
        <p className="rcpt-miss">Claude Code found one bug zerd missed: a refund applied before its capture.</p>
        <hr />
        <p className="rcpt-foot">one run each. not the same setup.<br />caveats below.</p>
      </div>
    </div>
  );
}
