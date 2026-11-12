// A tear-off line between sections, like the dashed edge on a coupon or a receipt.
export default function Perf() {
  return (
    <div className="perf wrap" aria-hidden="true">
      <span className="perf-cut">✂</span>
      <span className="perf-line" />
    </div>
  );
}
