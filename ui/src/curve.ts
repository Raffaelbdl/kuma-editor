// A grade's curves, as the compiler bends the picture with them: the same monotone cubic
// as `Curve` in `crates/kuma-core/src/grade.rs` (Fritsch–Carlson), so what the curve field
// draws is what renders. Both are pinned to the same values (`ui/test/curve.test.mjs`,
// `the_curve_draws_what_the_editor_draws` and `the_amount_curves_draw_what_the_editor_draws`):
// change one, change both.
//
// Pure, with no imports, so a test can load it without a browser.

/** A point of a curve, `[in, out]`, each 0..1 — or `[at, amount]`, amount -1..1. */
export type CurvePoint = [number, number];

/** What a curve is of: a tone curve (`in → out`, through the corners `[0, 0]` and `[1, 1]`),
 *  a curve of amounts against luma or saturation (0 for none, through `[0, 0]` and `[1, 0]`),
 *  or one of amounts against hue, which wraps round with no corners at all. */
export type CurveKind = "tone" | "amount" | "hue";

/** The curve of `kind` through `points` and the corners it is not told otherwise about (a
 *  tone curve's `[0, 0]` and `[1, 1]` unless points sit there), as the compiler reads it
 *  (`Curve::through`, `offset` and `periodic`); null when it changes nothing. */
export function curveOf(points: CurvePoint[], kind: CurveKind): ((v: number) => number) | null {
  if (kind === "hue") {
    if (points.every(([, y]) => y === 0)) return null;
    const f = monotone([-1, 0, 1].flatMap((turn) => points.map(([x, y]) => [x + turn, y] as CurvePoint)));
    return (v: number) => f(v - Math.floor(v));
  }
  if (points.length === 0) return null;
  const pts = withCorners(points, kind);
  if (pts.every(([x, y]) => (kind === "tone" ? x === y : y === 0))) return null;
  return monotone(pts);
}

function monotone(pts: CurvePoint[]): (v: number) => number {
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

/** Where a curve of `kind` ends when no point says otherwise: `[0, 0]`, and `[1, 1]` for a
 *  tone curve or `[1, 0]` for amounts. A curve against hue has no ends. */
export function corners(kind: CurveKind): [CurvePoint, CurvePoint] {
  return [[0, 0], [1, kind === "tone" ? 1 : 0]];
}

/** `points` with the corners the curve passes through added: what the curve field shows. */
export function withCorners(points: CurvePoint[], kind: CurveKind): CurvePoint[] {
  const pts = points.map((p) => [p[0], p[1]] as CurvePoint);
  if (kind === "hue") return pts;
  const [first, last] = corners(kind);
  if (pts.length === 0 || pts[0][0] > 0) pts.unshift(first);
  if (pts[pts.length - 1][0] < 1) pts.push(last);
  return pts;
}

/** `points` without the corners that are only what the curve would do anyway: what the
 *  document stores, so a curve someone only bent in the middle stays the points they set. */
export function withoutCorners(points: CurvePoint[], kind: CurveKind): CurvePoint[] {
  const pts = points.slice();
  if (kind === "hue") return pts;
  const [first, last] = corners(kind);
  const is = (p: CurvePoint, q: CurvePoint) => p[0] === q[0] && p[1] === q[1];
  if (pts.length && is(pts[0], first)) pts.shift();
  if (pts.length && is(pts[pts.length - 1], last)) pts.pop();
  return pts;
}

/** The colour of hue `h` on a hue curve's axis, as CSS: turns round the vectorscope from
 *  +Cb, as the grade reads hue — a mid-bright colour of that hue, for drawing the axis. */
export function hueColour(h: number): string {
  const a = h * 2 * Math.PI, y = 0.55, cb = 0.2 * Math.cos(a), cr = 0.2 * Math.sin(a);
  const r = y + 1.5748 * cr, b = y + 1.8556 * cb, g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
  const byte = (v: number) => Math.round(Math.min(1, Math.max(0, v)) * 255);
  return `rgb(${byte(r)}, ${byte(g)}, ${byte(b)})`;
}
