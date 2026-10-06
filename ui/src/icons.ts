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
  /** Panels held where they are, or free to move: a padlock shut and open. */
  lock: [
    shape("rect", { x: 5, y: 11, width: 14, height: 10, rx: 1.5 }),
    shape("path", { d: "M8 11V8a4 4 0 0 1 8 0v3", fill: "none", stroke: "currentColor", "stroke-width": 2 }),
  ],
  unlock: [
    shape("rect", { x: 5, y: 11, width: 14, height: 10, rx: 1.5 }),
    shape("path", { d: "M8 11V8a4 4 0 0 1 7.6-1.7", fill: "none", stroke: "currentColor", "stroke-width": 2 }),
  ],
  /** A keyframe: the diamond the strip and the clip draw keys with. Filled, an effect is
   *  keyframed; outlined, it could be. */
  key: [shape("polygon", { points: "12,3 21,12 12,21 3,12" })],
  "key-open": [shape("polygon", { points: "12,4.5 19.5,12 12,19.5 4.5,12", fill: "none", stroke: "currentColor", "stroke-width": 2.2, "stroke-linejoin": "round" })],
  /** Two links of a chain: a clip that moves with others. Stroked rather than filled,
   *  because a filled chain at 10px is a blob. */
  link: [
    shape("path", { d: "M10 14a4 4 0 0 0 6 0l3-3a4 4 0 0 0-6-6l-1 1", fill: "none", stroke: "currentColor", "stroke-width": 2.4, "stroke-linecap": "round" }),
    shape("path", { d: "M14 10a4 4 0 0 0-6 0l-3 3a4 4 0 0 0 6 6l1-1", fill: "none", stroke: "currentColor", "stroke-width": 2.4, "stroke-linecap": "round" }),
  ],
  /** A speaker with a cross after it: a clip muted on its own, while its track plays.
   *  Stroked, like `link`, so it reads at the 10px a clip's name is drawn at. */
  "volume-off": [
    shape("path", { d: "M4 9h4l5-4v14l-5-4H4z", fill: "none", stroke: "currentColor", "stroke-width": 2.2, "stroke-linejoin": "round" }),
    shape("path", { d: "M16 9l5 6M21 9l-5 6", fill: "none", stroke: "currentColor", "stroke-width": 2.4, "stroke-linecap": "round" }),
  ],
  /** The bin's toolbar. Stroked, like `link`, in one weight so the row reads as a set. */
  plus: [shape("path", { d: "M12 5v14M5 12h14", fill: "none", stroke: "currentColor", "stroke-width": 2.2, "stroke-linecap": "round", "stroke-linejoin": "round" })],
  "folder-plus": [shape("path", { d: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2zM12 10v6M9 13h6", fill: "none", stroke: "currentColor", "stroke-width": 2.2, "stroke-linecap": "round", "stroke-linejoin": "round" })],
  /** A file coming in: the arrow into the tray. */
  import: [shape("path", { d: "M12 3v12M7 10l5 5 5-5M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2", fill: "none", stroke: "currentColor", "stroke-width": 2.2, "stroke-linecap": "round", "stroke-linejoin": "round" })],
  /** A prompt: kept for the integrated terminal the bin's toolbar is to open. */
  terminal: [shape("path", { d: "M4 7l5 5-5 5M12 19h8", fill: "none", stroke: "currentColor", "stroke-width": 2.2, "stroke-linecap": "round", "stroke-linejoin": "round" })],
  refresh: [shape("path", { d: "M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7", fill: "none", stroke: "currentColor", "stroke-width": 2.2, "stroke-linecap": "round", "stroke-linejoin": "round" })],
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
