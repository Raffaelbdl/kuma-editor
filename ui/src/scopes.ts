// What the scopes panel draws, counted from a frame's pixels: a waveform of luma, the RGB
// parade, a vectorscope and a histogram — the four a grade is read with. Pure, with no
// imports, so a test can load it without a browser.

/** Rec. 709 luma weights, as the grade's maths uses them. */
const LUMA = [0.2126, 0.7152, 0.0722] as const;

export interface ScopeData {
  /** Columns the waveform and each parade channel are counted in. */
  columns: number;
  /** Luma by column: `columns × 256` counts, level 0 first. */
  waveform: Uint32Array;
  /** Red, green and blue by column, one `columns × 256` block after another. */
  parade: Uint32Array;
  /** Points a side of the vectorscope. */
  size: number;
  /** Chroma, `size × size` counts, row 0 at the top: Cb across, Cr up. */
  vector: Uint32Array;
  /** Red, green, blue and luma, 256 counts each. */
  histogram: Uint32Array;
}

/** Where a colour (0..255 each) sits on a vectorscope of `size` points: Rec. 709 Cb across
 *  and Cr up, each ±0.5 to the edge. */
export function vectorAt(r: number, g: number, b: number, size: number): [number, number] {
  const y = LUMA[0] * r + LUMA[1] * g + LUMA[2] * b;
  const cb = (b - y) / 1.8556 / 255;
  const cr = (r - y) / 1.5748 / 255;
  return [Math.round((cb + 0.5) * (size - 1)), Math.round((0.5 - cr) * (size - 1))];
}

/** Count `rgba` (`w × h` pixels, four bytes each) into every scope. */
export function analyse(rgba: Uint8Array | Uint8ClampedArray, w: number, h: number, columns = 256, size = 128): ScopeData {
  const waveform = new Uint32Array(columns * 256);
  const parade = new Uint32Array(3 * columns * 256);
  const vector = new Uint32Array(size * size);
  const histogram = new Uint32Array(4 * 256);
  const block = columns * 256;
  for (let py = 0; py < h; py++) {
    for (let px = 0; px < w; px++) {
      const i = (py * w + px) * 4;
      const r = rgba[i], g = rgba[i + 1], b = rgba[i + 2];
      const y = Math.round(LUMA[0] * r + LUMA[1] * g + LUMA[2] * b);
      const col = Math.min(columns - 1, Math.floor((px * columns) / w));
      waveform[col * 256 + y]++;
      parade[col * 256 + r]++;
      parade[block + col * 256 + g]++;
      parade[2 * block + col * 256 + b]++;
      const [vx, vy] = vectorAt(r, g, b, size);
      vector[vy * size + vx]++;
      histogram[r]++;
      histogram[256 + g]++;
      histogram[512 + b]++;
      histogram[768 + y]++;
    }
  }
  return { columns, waveform, parade, size, vector, histogram };
}

/** The angle of the skin-tone line on the vectorscope, degrees anticlockwise from +Cb: where
 *  faces of every complexion fall, so a cast on skin reads at a glance. */
export const SKIN_ANGLE = 123;

/** The 75% colour bars' primaries and secondaries, as vectorscope targets. */
export const TARGETS: { name: string; rgb: [number, number, number] }[] = [
  { name: "R", rgb: [191, 0, 0] }, { name: "Mg", rgb: [191, 0, 191] }, { name: "B", rgb: [0, 0, 191] },
  { name: "Cy", rgb: [0, 191, 191] }, { name: "G", rgb: [0, 191, 0] }, { name: "Yl", rgb: [191, 191, 0] },
];
