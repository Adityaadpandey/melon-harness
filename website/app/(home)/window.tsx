import type { ReactNode } from "react";

// A plain terminal frame: a title line and whatever lives inside. No traffic lights.
export default function AppWindow({ title, end, children, className = "" }: {
  title?: string; end?: ReactNode; children: ReactNode; className?: string;
}) {
  return (
    <div className={`win ${className}`}>
      <div className="win-bar"><span className="win-title">{title}</span><span className="win-end">{end}</span></div>
      {children}
    </div>
  );
}
