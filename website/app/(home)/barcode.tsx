// A barcode made from the text it carries, so the bars are the install command and not decoration.
// Each character picks a bar width and a gap width from its char code.
export default function Barcode({ text }: { text: string }) {
  let x = 0;
  const bars: [number, number][] = [];
  for (const ch of text) {
    const c = ch.charCodeAt(0);
    x += 1 + (c >> 3) % 3;
    const w = 1 + (c % 4);
    bars.push([x, w]);
    x += w;
  }
  return (
    <svg className="barcode" viewBox={`0 0 ${x + 2} 40`} preserveAspectRatio="none" role="img" aria-label={`Barcode of: ${text}`}>
      {bars.map(([bx, w], i) => <rect key={i} x={bx} y="0" width={w} height="40" fill="currentColor" />)}
    </svg>
  );
}
