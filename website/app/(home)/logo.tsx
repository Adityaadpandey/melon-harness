import Link from "next/link";

// The zerd mark is a receipt with a tick on it. The tick redraws itself when you hover the logo.
const PATH = "M5 2H23V24L20 21.8L17 24L14 21.8L11 24L8 21.8L5 24Z";
export function Mark({ size = 26 }: { size?: number }) {
  return (
    <svg className="mark" width={Math.round(size * 0.77)} height={size} viewBox="4 1 20 24" aria-hidden="true" focusable="false">
      <path d={PATH} fill="#f7f2ee" />
      <path className="mark-tick" d="M9.6 11.6L12.6 14.6L18.4 8.4" fill="none" stroke="#07060f" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" pathLength="1" />
    </svg>
  );
}

export default function Logo() {
  return <Link href="/" className="logo-lp" aria-label="zerd, home"><Mark /><span>zerd</span></Link>;
}
