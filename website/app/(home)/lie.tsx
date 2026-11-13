"use client";
import { useState } from "react";

// The same final answer, shown twice: as most agents leave it, and after zerd's checks.
export default function Lie() {
  const [on, setOn] = useState(false);
  return (
    <div className={`lie${on ? " on" : ""}`}>
      <div className="lie-switch" role="radiogroup" aria-label="Who checked this answer">
        <button type="button" role="radio" aria-checked={!on} onClick={() => setOn(false)}>most agents</button>
        <button type="button" role="radio" aria-checked={on} onClick={() => setOn(true)}>zerd</button>
      </div>

      <div className="lie-answer" aria-live="polite">
        <span className={on ? "stamp stamp-green" : "stamp stamp-red"} key={on ? "y" : "n"} aria-hidden="true">{on ? "checked" : "unchecked"}</span>
        <div className="lie-head"><i className={on ? "dot ok" : "dot"} />{on ? "zerd: checked before answering" : "most agents: nothing checked"}</div>
        <p>
          Fixed the redirect loop. The refresh ran twice in{" "}
          <span className="claim">
            <span className="claim-text">src/auth/session.rs:88</span>
            <span className="tag tag-bad">not found, removed</span>
          </span>
          , so I made it run once.
        </p>
        <p>
          <span className="claim">
            <span className="claim-text">All tests pass</span>
            <span className="mark" aria-hidden="true">{on ? "–" : "✓"}</span>
            <span className="tag tag-warn">not run</span>
          </span>
        </p>
      </div>

      <div className="receipt-wrap" aria-hidden={!on}>
        <div className="receipt-box">
          <div className="rc">
            <p><span className="t-bad">✗</span> src/auth/session.rs:88 <span className="t-dim">looked up on disk: no such file, removed from the answer</span></p>
            <p><span className="t-warn">–</span> all tests pass <span className="t-dim">no test command ran in this session, so it says not run</span></p>
            <p><span className="t-ok">verified</span>  1 claim removed · 1 claim marked not run</p>
          </div>
        </div>
      </div>
    </div>
  );
}
