// Colour as the inspector's colour fields edit it: CSS hex with alpha, the hue/saturation/
// value a picker drags, and the plane a grade's colour wheel is. Pure, with no imports, so a
// test can load it without a browser.

/** A colour as 0..255 channels; `a` 255 is opaque. */
export interface Rgba { r: number; g: number; b: number; a: number }

/** `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` (the `#` optional), or null — the forms
 *  `is_css_color` in `kuma-core` takes, plus `#rgba`. */
export function parseHex(css: string): Rgba | null {
  const hex = css.trim().replace(/^#/, "");
  if (!/^[0-9a-f]+$/i.test(hex) || ![3, 4, 6, 8].includes(hex.length)) return null;
  const short = hex.length <= 4;
  const at = (i: number) => (short ? parseInt(hex[i] + hex[i], 16) : parseInt(hex.slice(2 * i, 2 * i + 2), 16));
  const four = hex.length === 4 || hex.length === 8;
  return { r: at(0), g: at(1), b: at(2), a: four ? at(3) : 255 };
}

/** `#rrggbb`, or `#rrggbbaa` when it is not opaque: the shortest form that says it. */
export function toHex(c: Rgba): string {
  const two = (v: number) => Math.round(Math.min(255, Math.max(0, v))).toString(16).padStart(2, "0");
  return `#${two(c.r)}${two(c.g)}${two(c.b)}${Math.round(c.a) >= 255 ? "" : two(c.a)}`;
}

/** Hue in degrees 0..360, saturation and value 0..1. */
export interface Hsv { h: number; s: number; v: number }

export function rgbToHsv({ r, g, b }: Rgba): Hsv {
  const [R, G, B] = [r / 255, g / 255, b / 255];
  const max = Math.max(R, G, B), min = Math.min(R, G, B), d = max - min;
  let h = 0;
  if (d > 0) {
    if (max === R) h = ((G - B) / d) % 6;
    else if (max === G) h = (B - R) / d + 2;
    else h = (R - G) / d + 4;
    h = (h * 60 + 360) % 360;
  }
  return { h, s: max === 0 ? 0 : d / max, v: max };
}

export function hsvToRgb({ h, s, v }: Hsv, a = 255): Rgba {
  const f = (n: number) => {
    const k = (n + h / 60) % 6;
    return Math.round((v - v * s * Math.max(0, Math.min(k, 4 - k, 1))) * 255);
  };
  return { r: f(5), g: f(3), b: f(1), a };
}

/** A grade wheel's three channel offsets. */
export interface Offsets { r: number; g: number; b: number }

/** Where offsets sit on a colour wheel: their shared part is `master` (moving all three
 *  alike is brightness, not colour), and what is left is a point in the plane — red to the
 *  right, green up and to the left, blue down and to the left, a third of a turn apart. */
export function offsetsToWheel(o: Offsets): { x: number; y: number; master: number } {
  const master = (o.r + o.g + o.b) / 3;
  const [r, g, b] = [o.r - master, o.g - master, o.b - master];
  return { x: r - (g + b) / 2, y: (Math.sqrt(3) / 2) * (g - b), master };
}

/** The offsets at a point of the wheel, with `master` added to each. */
export function wheelToOffsets(x: number, y: number, master: number): Offsets {
  const r = (2 / 3) * x;
  const g = -x / 3 + y / Math.sqrt(3);
  const b = -x / 3 - y / Math.sqrt(3);
  return { r: r + master, g: g + master, b: b + master };
}
