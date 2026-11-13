import type { ReactNode } from "react";

// A desktop app window: traffic lights, a title bar, then whatever lives inside.
// `bar` replaces the plain title (the replay puts its tabs there), `end` sits on the right.
export default function AppWindow({ title, bar, end, children, className = "" }: {
  title?: string; bar?: ReactNode; end?: ReactNode; children: ReactNode; className?: string;
}) {
  return (
    <div className={`win ${className}`}>
      <div className="win-bar">
        <span className="win-lights" aria-hidden="true"><i /><i /><i /></span>
        {bar ?? <span className="win-title">{title}</span>}
        <span className="win-end">{end}</span>
      </div>
      {children}
    </div>
  );
}
