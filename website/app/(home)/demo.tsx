"use client";
import { useEffect, useRef, useState } from "react";
import AppWindow from "./window";

// A real recording: the zerd binary run in a terminal on a small demo repo, saved with its real
// timing (public/demo/zerd-run.cast) and played back by asciinema-player. Nothing here is scripted.
// Long waits are shortened by the player (idleTimeLimit); the output itself is untouched.
type Player = { play(): Promise<unknown>; pause(): Promise<unknown>; seek(t: number | string): Promise<unknown>; dispose(): void; addEventListener(e: string, f: () => void): void };
type Api = { create(src: string, el: HTMLElement, opts: Record<string, unknown>): Player };
declare global { interface Window { AsciinemaPlayer?: Api } }

const CAST = "/demo/zerd-run.cast";
const LIB = "/demo/asciinema-player.min.js";
const CSS = "/demo/asciinema-player.css";

function loadLib(): Promise<Api> {
  if (window.AsciinemaPlayer) return Promise.resolve(window.AsciinemaPlayer);
  return new Promise((ok, fail) => {
    const s = document.createElement("script");
    s.src = LIB;
    s.onload = () => (window.AsciinemaPlayer ? ok(window.AsciinemaPlayer) : fail(new Error("player missing")));
    s.onerror = () => fail(new Error("player failed to load"));
    document.head.appendChild(s);
  });
}

export default function Demo() {
  const root = useRef<HTMLDivElement>(null);
  const stage = useRef<HTMLDivElement>(null);
  const player = useRef<Player | null>(null);
  const [playing, setPlaying] = useState(false);
  const [failed, setFailed] = useState(false);
  const reduced = useRef(false);

  useEffect(() => {
    reduced.current = matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (!document.querySelector(`link[href="${CSS}"]`)) {
      const l = document.createElement("link");
      l.rel = "stylesheet";
      l.href = CSS;
      document.head.appendChild(l);
    }
    let gone = false;
    let io: IntersectionObserver | undefined;
    loadLib().then((api) => {
      if (gone || !stage.current) return;
      const p = api.create(CAST, stage.current, {
        loop: true, idleTimeLimit: 1.4, controls: false, fit: "width", theme: "zerd",
        terminalFontFamily: "var(--font-mono), 'JetBrains Mono', ui-monospace, monospace", terminalLineHeight: 1.2,
        poster: "npt:0:38", preload: true,
      });
      player.current = p;
      p.addEventListener("play", () => setPlaying(true));
      p.addEventListener("pause", () => setPlaying(false));
      // Starts when it scrolls into view, pauses when it leaves. Reduced motion keeps the finished frame.
      let started = false;
      io = new IntersectionObserver(([en]) => {
        if (reduced.current) return;
        if (!en.isIntersecting) { if (started) p.pause(); return; }
        // the first play starts from the top, not from the finished frame the poster shows
        if (started) p.play(); else { started = true; p.seek(0).then(() => p.play()); }
      }, { threshold: 0.35 });
      if (root.current) io.observe(root.current);
    }).catch(() => setFailed(true));
    return () => { gone = true; io?.disconnect(); player.current?.dispose(); player.current = null; };
  }, []);

  const toggle = () => (playing ? player.current?.pause() : player.current?.play());
  const replay = async () => { await player.current?.seek(0); player.current?.play(); };

  const controls = (
    <>
      <span className="live"><i className={playing ? "on" : ""} aria-hidden="true" />{playing ? "recording playing" : "recording paused"}</span>
      <button type="button" onClick={toggle} disabled={!player.current}>{playing ? "Pause" : "Play"}</button>
      <button type="button" onClick={replay} disabled={!player.current}>Replay</button>
    </>
  );
  return (
    <div className="demo" ref={root}>
      <AppWindow title="zerd" end={controls}>
        <div className="stage" ref={stage} role="img" aria-label="A real recording of zerd fixing a retry bug in a small Python project, then running its tests with /verify">
          {failed && <p className="stage-fail">The recording could not load. The same run is described below.</p>}
        </div>
      </AppWindow>
      <p className="note">A real recording: zerd with MiniMax M3 fixing a bug in a small demo repo, then running its own checks with /verify. Not scripted. Long pauses are shortened.</p>
    </div>
  );
}
