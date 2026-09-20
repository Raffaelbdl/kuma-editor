// Icons, as inline SVG.
//
// The transport's glyphs used to be characters, and they never matched each other: `⏮` is
// U+23EE, which has *emoji* presentation, so the browser reached past the UI font for the
// colour emoji one and drew a bigger, differently weighted glyph beside a monochrome
// triangle. No amount of CSS fixes that — which font drew a character is not ours to pick,
// and it differs per machine. A shape is ours.
//
// So: paths in a 24×24 box, filled with `currentColor`, built as nodes rather than markup
// (never `innerHTML`, for the reason `dom.ts` gives). There are only the ones something
// uses. An icon pack — Lucide, Feather, Phosphor, all plain SVG and usable without a
// framework — drops in behind `icon()` without touching a caller, and is worth the
// dependency the day this wants a dozen.

const NS = "http://www.w3.org/2000/svg";

type Shape = [tag: string, attrs: Record<string, string | number>];
const shape = (tag: string, attrs: Record<string, string | number>): Shape => [tag, attrs];

const ICONS = {
  play: [shape("polygon", { points: "7,4 20,12 7,20" })],
  pause: [shape("rect", { x: 6, y: 4, width: 4, height: 16, rx: 1 }), shape("rect", { x: 14, y: 4, width: 4, height: 16, rx: 1 })],
  /** To the start: the bar it stops against, then the triangle running into it. */
  "skip-back": [shape("rect", { x: 5, y: 4, width: 3, height: 16, rx: 1 }), shape("polygon", { points: "20,4 20,20 9,12" })],
  /** Step back and forward — over keyframes, on the buttons beside a keyframe strip. */
  "chevron-left": [shape("polygon", { points: "16,4 18,6 12,12 18,18 16,20 8,12" })],
  "chevron-right": [shape("polygon", { points: "8,4 6,6 12,12 6,18 8,20 16,12" })],
  /** Appearance: a disc with one half filled, the glyph every application uses for
   *  "how this looks". Drawn as a stroked circle plus a half disc so it reads at 14px,
   *  where a painter's palette is a smudge. */
  palette: [
    shape("circle", { cx: 12, cy: 12, r: 9, fill: "none", stroke: "currentColor", "stroke-width": 2 }),
    shape("path", { d: "M12 3a9 9 0 0 0 0 18z" }),
  ],
  /** Two links of a chain: a clip that moves with others. Stroked rather than filled,
   *  because a filled chain at 10px is a blob. */
  link: [
    shape("path", { d: "M10 14a4 4 0 0 0 6 0l3-3a4 4 0 0 0-6-6l-1 1", fill: "none", stroke: "currentColor", "stroke-width": 2.4, "stroke-linecap": "round" }),
    shape("path", { d: "M14 10a4 4 0 0 0-6 0l-3 3a4 4 0 0 0 6 6l1-1", fill: "none", stroke: "currentColor", "stroke-width": 2.4, "stroke-linecap": "round" }),
  ],
};

export type IconName = keyof typeof ICONS;

/** An SVG element with its attributes — the namespaced `el`, for the icons here and the
 *  odd graph elsewhere. */
export function svgEl<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number> = {}): SVGElementTagNameMap[K] {
  const node = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
  return node;
}

/** One icon, `size` CSS pixels square, in the colour of the text around it. */
export function icon(name: IconName, size = 14): SVGSVGElement {
  // Decoration (`aria-hidden`): what the button does is in its `title`, and a screen
  // reader reading "image" between them would only get in the way.
  const svg = svgEl("svg", { viewBox: "0 0 24 24", width: size, height: size, fill: "currentColor", "aria-hidden": "true" });
  for (const [tag, attrs] of ICONS[name]) svg.appendChild(svgEl(tag as keyof SVGElementTagNameMap, attrs));
  return svg;
}
