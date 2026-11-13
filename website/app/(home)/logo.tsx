import Link from "next/link";

// zerd has no logo, only the wordmark: the name plus a small square that stands for the tick of a passed check.
export default function Logo() {
  return (
    <Link href="/" className="logo-lp" aria-label="zerd, home">
      <span>zerd</span>
      <i className="logo-tick" aria-hidden />
    </Link>
  );
}
