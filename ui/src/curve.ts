// A grade's tone curve, as the compiler bends the picture with it: the same monotone cubic
// as `Curve` in `crates/kuma-core/src/grade.rs` (Fritsch–Carlson), so what the curve field
// draws is what renders. Both are pinned to the same values (`ui/test/curve.test.mjs` and
// `the_curve_draws_what_the_editor_draws`): change one, change both.
//
// Pure, with no imports, so a test can load it without a browser.

/** A point of a curve, `[in, out]`, each 0..1. */
export type CurvePoint = [number, number];

/** The curve through `points` and the corners it is not told otherwise about — `[0, 0]`
 *  unless a point sits at in 0, `[1, 1]` unless one sits at in 1 — as a function, or null
 *  when that is the straight line. */
export function curveThrough(points: CurvePoint[]): ((v: number) => number) | null {
  if (points.length === 0) return null;
  const pts = withCorners(points);
  if (pts.every(([x, y]) => x === y)) return null;
  const x = pts.map((p) => p[0]);
  const y = pts.map((p) => p[1]);
  const n = x.length;
  const d = Array.from({ length: n - 1 }, (_, k) => (y[k + 1] - y[k]) / (x[k + 1] - x[k]));
  const m = new Array<number>(n).fill(0);
  m[0] = d[0];
  m[n - 1] = d[n - 2];
  for (let k = 1; k < n - 1; k++) m[k] = d[k - 1] * d[k] > 0 ? (d[k - 1] + d[k]) / 2 : 0;
  for (let k = 0; k < n - 1; k++) {
    if (d[k] === 0) {
      m[k] = 0;
      m[k + 1] = 0;
      continue;
    }
    const a = m[k] / d[k], b = m[k + 1] / d[k];
    const h = a * a + b * b;
    if (h > 9) {
      const t = 3 / Math.sqrt(h);
      m[k] = t * a * d[k];
      m[k + 1] = t * b * d[k];
    }
  }
  return (v: number) => {
    if (v <= x[0]) return y[0] + (v - x[0]) * m[0];
    if (v >= x[n - 1]) return y[n - 1] + (v - x[n - 1]) * m[n - 1];
    let k = 0;
    while (x[k + 1] <= v) k++;
    const h = x[k + 1] - x[k];
    const t = (v - x[k]) / h, t2 = t * t, t3 = t2 * t;
    return (2 * t3 - 3 * t2 + 1) * y[k] + (t3 - 2 * t2 + t) * h * m[k] + (-2 * t3 + 3 * t2) * y[k + 1] + (t3 - t2) * h * m[k + 1];
  };
}

/** `points` with the corners the curve passes through added: what the curve field shows. */
export function withCorners(points: CurvePoint[]): CurvePoint[] {
  const pts = points.map((p) => [p[0], p[1]] as CurvePoint);
  if (pts.length === 0 || pts[0][0] > 0) pts.unshift([0, 0]);
  if (pts[pts.length - 1][0] < 1) pts.push([1, 1]);
  return pts;
}

/** `points` without the corners that are only what the curve would do anyway: what the
 *  document stores, so a curve someone only bent in the middle stays the points they set. */
export function withoutCorners(points: CurvePoint[]): CurvePoint[] {
  const pts = points.slice();
  if (pts.length && pts[0][0] === 0 && pts[0][1] === 0) pts.shift();
  if (pts.length && pts[pts.length - 1][0] === 1 && pts[pts.length - 1][1] === 1) pts.pop();
  return pts;
}
