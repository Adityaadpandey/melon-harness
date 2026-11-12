"use client";
import { useEffect, useRef, useState } from "react";

// One line of the replay. k picks the colour, id lets a later event replace an earlier line
// (a "running" line turning into "passed").
type Ev = { d: number; k: string; a?: string; b?: string; id?: string };
type Scn = { tab: string; prompt: string; label: string; ev: Ev[] };

const checks = (files: string): Ev[] => [
  { d: 500, k: "head", a: "checks on the changed files" },
  { d: 350, k: "run", a: `Biome check on ${files}`, id: "biome" },
  { d: 1300, k: "ok", a: `Biome check on ${files}`, b: "passed", id: "biome" },
  { d: 250, k: "run", a: "TypeScript check", id: "tsc" },
  { d: 1500, k: "ok", a: "TypeScript check", b: "passed", id: "tsc" },
];

const SCENARIOS: Scn[] = [
  {
    tab: "Fix a bug",
    prompt: "fix the redirect loop in RouteProgress",
    label: "zerd fixes a redirect loop, runs two checks, drops one review finding and removes one invented path",
    ev: [
      { d: 600, k: "tool", a: "read", b: "src/components/RouteProgress.tsx" },
      { d: 450, k: "tool", a: "read", b: "src/app/layout.tsx" },
      { d: 700, k: "tool", a: "advisor", b: "a stronger model reads the transcript" },
      { d: 800, k: "tool", a: "edit", b: "src/components/RouteProgress.tsx" },
      { d: 250, k: "dim", a: "  - router.replace(pathname)" },
      { d: 120, k: "dim", a: "  + if (pathname === last.current) return; last.current = pathname" },
      { d: 450, k: "tool", a: "edit", b: "src/hooks/use-route.ts" },
      { d: 450, k: "tool", a: "edit", b: "src/app/layout.tsx" },
      ...checks("3 changed files"),
      { d: 500, k: "run", a: "Independent review of the diff, by a second model", id: "rev" },
      { d: 1700, k: "ok", a: "Independent review of the diff, by a second model", b: "1 finding", id: "rev" },
      { d: 300, k: "warn", a: "finding", b: "start() can fire twice when the layout remounts" },
      { d: 600, k: "run", a: "verifier is trying to refute it", id: "ver" },
      { d: 1700, k: "ok", a: "verifier could not reproduce it", b: "finding dropped", id: "ver" },
      { d: 700, k: "ans", a: "Fixed. The loop came from comparing against a stale value in src/components/RouteProgress.tsx:42, and the layout needed the same guard at src/app/layout.tsx:17. Session reuse in src/auth/session.rs:88 was already fine." },
      { d: 700, k: "head", a: "looking up every path:line in the answer" },
      { d: 450, k: "ok", a: "src/components/RouteProgress.tsx:42" },
      { d: 350, k: "ok", a: "src/app/layout.tsx:17" },
      { d: 500, k: "bad", a: "src/auth/session.rs:88", b: "not found, removed from the answer" },
      { d: 500, k: "dim", a: "Worked for 11s · ↓ 0.3k tokens" },
      { d: 300, k: "foot", a: "verified", b: "3 files · 2 checks ok · 1 finding dropped" },
    ],
  },
  {
    tab: "Review a diff",
    prompt: "review my changes",
    label: "zerd reviews a working-tree diff with a second model, keeps one finding and drops one",
    ev: [
      { d: 600, k: "tool", a: "git", b: "diff of the working tree" },
      { d: 600, k: "dim", a: "scope: working tree, nothing fetched" },
      { d: 600, k: "run", a: "Independent review of the real diff, by a second model", id: "rev" },
      { d: 2000, k: "ok", a: "Independent review of the real diff, by a second model", b: "2 findings", id: "rev" },
      { d: 300, k: "warn", a: "finding 1", b: "retry loop never backs off after a 429" },
      { d: 200, k: "warn", a: "finding 2", b: "cache key ignores the tenant id" },
      { d: 600, k: "run", a: "verifier is trying to refute each one", id: "ver" },
      { d: 2000, k: "ok", a: "finding 1 holds up under a second look", b: "kept", id: "ver" },
      { d: 500, k: "ok", a: "finding 2 refuted: the tenant id is part of the key two lines up", b: "dropped", id: "ver2" },
      { d: 700, k: "ans", a: "One real problem: the retry in src/upload/client.ts:57 hammers the server after a 429. The cache-key worry was a false alarm. I also remember a helper in src/upload/legacy.ts:210 that has the same bug." },
      { d: 700, k: "head", a: "looking up every path:line in the answer" },
      { d: 450, k: "ok", a: "src/upload/client.ts:57" },
      { d: 500, k: "bad", a: "src/upload/legacy.ts:210", b: "not found, removed from the answer" },
      { d: 500, k: "dim", a: "Worked for 9s · ↓ 0.2k tokens" },
      { d: 300, k: "foot", a: "verified", b: "1 finding kept · 1 finding dropped · 1 path removed" },
    ],
  },
  {
    tab: "Provider hits its quota",
    prompt: "add a retry to the upload client",
    label: "zerd keeps working when a provider runs out of quota, fails over to another one and says so",
    ev: [
      { d: 600, k: "tool", a: "read", b: "src/upload/client.ts" },
      { d: 800, k: "bad", a: "claude", b: "plan quota reached" },
      { d: 600, k: "warn", a: "failover", b: "switching to gpt, claude is blocked for this session" },
      { d: 900, k: "tool", a: "edit", b: "src/upload/client.ts" },
      { d: 250, k: "dim", a: "  + for (let i = 0; i < 3; i++) { try { return await send(f) } catch {} }" },
      { d: 450, k: "tool", a: "edit", b: "src/upload/client.test.ts" },
      ...checks("2 changed files"),
      { d: 500, k: "run", a: "Independent review of the diff, by a second model", id: "rev" },
      { d: 1600, k: "ok", a: "Independent review of the diff, by a second model", b: "no findings", id: "rev" },
      { d: 600, k: "ans", a: "Added a three-try retry in src/upload/client.ts:31 and a test next to it. The provider changed halfway through; the details are in the trace." },
      { d: 700, k: "head", a: "looking up every path:line in the answer" },
      { d: 450, k: "ok", a: "src/upload/client.ts:31" },
      { d: 500, k: "dim", a: "Worked for 14s · ↓ 0.4k tokens" },
      { d: 300, k: "foot", a: "verified", b: "2 files · 2 checks ok · failed over once" },
    ],
  },
];

function lines(ev: Ev[], upto: number) {
  const out: Ev[] = [];
  const at: Record<string, number> = {};
  ev.slice(0, upto).forEach((e) => {
    if (e.id && e.id in at) out[at[e.id]] = e;
    else {
      if (e.id) at[e.id] = out.length;
      out.push(e);
    }
  });
  return out;
}

function Line({ e }: { e: Ev }) {
  switch (e.k) {
    case "tool": return <div className="tl"><span className="t-blue">● {e.a}</span> <span className="t-dim">{e.b}</span></div>;
    case "run": return <div className="tl"><span className="spin" aria-hidden="true" /> {e.a}</div>;
    case "ok": return <div className="tl"><span className="t-ok">✓</span> {e.a}{e.b && <span className="t-dim">: {e.b}</span>}</div>;
    case "bad": return <div className="tl"><span className="t-bad">✗ {e.a}</span> <span className="t-dim">{e.b}</span></div>;
    case "warn": return <div className="tl"><span className="t-warn">! {e.a}</span> <span className="t-dim">{e.b}</span></div>;
    case "head": return <div className="tl t-gap t-dim">{e.a}</div>;
    case "ans": return <div className="tl t-gap t-ans">{e.a}</div>;
    case "foot": return <div className="tl t-foot"><span className="t-ok">{e.a}</span>  {e.b}</div>;
    default: return <div className="tl t-dim">{e.a}</div>;
  }
}

export default function Demo() {
  const [si, setSi] = useState(0);
  const scn = SCENARIOS[si];
  // Server render shows the finished state, so it reads fine without any script.
  const [typed, setTyped] = useState(scn.prompt.length);
  const [step, setStep] = useState(scn.ev.length);
  const [playing, setPlaying] = useState(true);
  const [seen, setSeen] = useState(false);
  const [tabOpen, setTabOpen] = useState(true);
  const [reduced, setReduced] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const body = useRef<HTMLDivElement>(null);

  // Work out reduced motion and start the first run. Reduced motion keeps the finished state.
  useEffect(() => {
    const r = matchMedia("(prefers-reduced-motion: reduce)").matches;
    setReduced(r);
    if (!r) { setTyped(0); setStep(0); }
  }, []);

  useEffect(() => {
    const io = new IntersectionObserver(([en]) => setSeen(en.isIntersecting), { threshold: 0.15 });
    if (root.current) io.observe(root.current);
    const vis = () => setTabOpen(!document.hidden);
    document.addEventListener("visibilitychange", vis);
    vis();
    return () => { io.disconnect(); document.removeEventListener("visibilitychange", vis); };
  }, []);

  const active = playing && seen && tabOpen && !reduced;
  const done = typed >= scn.prompt.length && step >= scn.ev.length;

  // One timer at a time: type the prompt, then each event after its own delay, then loop.
  useEffect(() => {
    if (!active) return;
    let t: ReturnType<typeof setTimeout>;
    if (typed < scn.prompt.length) t = setTimeout(() => setTyped(typed + 1), typed === 0 ? 700 : 42);
    else if (step < scn.ev.length) t = setTimeout(() => setStep(step + 1), step === 0 ? 700 : scn.ev[step].d);
    else t = setTimeout(() => { setTyped(0); setStep(0); }, 5000);
    return () => clearTimeout(t);
  }, [active, typed, step, scn]);

  useEffect(() => {
    const el = body.current;
    if (el && !reduced) el.scrollTop = el.scrollHeight;
  }, [step, typed, reduced]);

  const pick = (i: number) => {
    setSi(i);
    setPlaying(true);
    if (reduced) { setTyped(SCENARIOS[i].prompt.length); setStep(SCENARIOS[i].ev.length); }
    else { setTyped(0); setStep(0); }
  };
  const replay = () => {
    setPlaying(true);
    setTyped(0);
    setStep(0);
  };
  const onKey = (e: React.KeyboardEvent) => {
    const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!d) return;
    e.preventDefault();
    const n = (si + d + SCENARIOS.length) % SCENARIOS.length;
    pick(n);
    (e.currentTarget.children[n] as HTMLElement).focus();
  };

  const shown = lines(scn.ev, step);
  return (
    <div className="demo" ref={root}>
      <div className="tabs" role="tablist" aria-label="Choose a scenario" onKeyDown={onKey}>
        {SCENARIOS.map((s, i) => (
          <button key={s.tab} role="tab" id={`demo-tab-${i}`} aria-selected={i === si} aria-controls="demo-panel" tabIndex={i === si ? 0 : -1} onClick={() => pick(i)}>
            {s.tab}
          </button>
        ))}
      </div>
      <div className="win-term" role="tabpanel" id="demo-panel" aria-labelledby={`demo-tab-${si}`}>
        <div className="bar">
          <span className="dots" aria-hidden="true"><i /><i /><i /></span>
          <span className="live"><i className={active ? "on" : ""} aria-hidden="true" />{active ? "replaying" : done ? "finished" : "paused"}</span>
          <span className="spacer" />
          <button type="button" onClick={() => setPlaying(!playing)} disabled={reduced} aria-pressed={!playing}>{playing ? "Pause" : "Play"}</button>
          <button type="button" onClick={replay} disabled={reduced}>Replay</button>
        </div>
        <div className="body" ref={body} role="region" aria-label={scn.label} aria-live="off" tabIndex={0}>
          <div className="tl"><span className="t-blue">zerd</span> <span className="t-dim">&gt;</span> {scn.prompt.slice(0, typed)}{(typed < scn.prompt.length || done) && <span className="caret" aria-hidden="true" />}</div>
          {shown.map((e, i) => <Line e={e} key={e.id ?? i} />)}
          {!done && typed >= scn.prompt.length && <div className="tl"><span className="caret" aria-hidden="true" /></div>}
        </div>
      </div>
      <p className="note">Replay of a scripted session. It follows what zerd really does; it is not a live run.</p>
    </div>
  );
}
