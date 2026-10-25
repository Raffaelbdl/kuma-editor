// `s` — scopes: the waveform, the RGB parade, the vectorscope and the histogram of what the
// preview shows — the engine's frame, or the grade being dragged while it is (`gradepreview.ts`)
// — so a grade is read the way a colourist reads one, as it is made. The agent's equivalent
// is `measure_color`, in numbers.
//
// It reads the preview of this window: in a window of its own there is no frame to read, and
// it says so. It counts a frame at most once an animation frame, and not at all while hidden.

import { el, selectInput } from "../dom";
import { analyse, SKIN_ANGLE, TARGETS, vectorAt, type ScopeData } from "../scopes";

type Kind = "all" | "waveform" | "parade" | "vectorscope" | "histogram";
const KINDS: { value: Kind; label: string }[] = [
  { value: "all", label: "all four" }, { value: "waveform", label: "waveform" }, { value: "parade", label: "RGB parade" },
  { value: "vectorscope", label: "vectorscope" }, { value: "histogram", label: "histogram" },
];
/** Which the panel shows: view state, this browser's. */
const SHOWN = "kuma.scopes";

/** How wide the frame is counted at: enough for a scope, a fraction of the work of the whole. */
const READ_WIDTH = 320;
const COLUMNS = 160;
const VECTOR = 128;

export function scopesPanel(): { body: HTMLElement; tools: HTMLElement; show: () => void } {
  const body = el("div", "scopes-panel");
  let kind: Kind = "all";
  try { kind = (localStorage.getItem(SHOWN) as Kind) || "all"; } catch { /* storage off: the default */ }
  const tools = el("span", "lib-tools");
  tools.appendChild(selectInput(KINDS, kind, (v) => {
    kind = v as Kind;
    try { localStorage.setItem(SHOWN, kind); } catch { /* not kept */ }
    layout();
    update();
  }));

  const scope = (name: string, w: number, h: number) => {
    const canvas = el("canvas", `scope scope-${name}`);
    canvas.width = w;
    canvas.height = h;
    const box = el("figure", "scope-box");
    box.append(canvas, el("figcaption", "muted tiny", name));
    return { box, canvas, ctx: canvas.getContext("2d")! };
  };
  const views = {
    waveform: scope("waveform", COLUMNS, 256),
    parade: scope("parade", COLUMNS * 3, 256),
    vectorscope: scope("vectorscope", VECTOR, VECTOR),
    histogram: scope("histogram", 256, 128),
  };
  const empty = el("p", "muted tiny", "No frame to read: the scopes read the preview in this window.");
  const grid = el("div", "scopes-grid");
  const layout = () => {
    grid.replaceChildren(...(kind === "all" ? Object.values(views) : [views[kind]]).map((v) => v.box));
    grid.classList.toggle("one", kind !== "all");
  };
  layout();
  body.append(grid, empty);

  const scratch = document.createElement("canvas");
  const sctx = scratch.getContext("2d", { willReadFrequently: true })!;
  /** The pixels the preview shows now, at READ_WIDTH, or null when there is no frame. */
  const read = (): ImageData | null => {
    const st = document.querySelector("#preview .stage");
    const live = st?.querySelector<HTMLCanvasElement>("canvas.grade-live:not([hidden])");
    const src = live ?? st?.querySelector<HTMLCanvasElement>("canvas:not(.grade-live)");
    if (!src || !src.width || !src.height) return null;
    const w = Math.min(src.width, READ_WIDTH), h = Math.max(1, Math.round((src.height * w) / src.width));
    scratch.width = w;
    scratch.height = h;
    sctx.drawImage(src, 0, 0, w, h);
    return sctx.getImageData(0, 0, w, h);
  };

  let queued = false;
  const update = () => {
    if (queued || !body.isConnected || !body.offsetWidth) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      const img = read();
      empty.hidden = !!img;
      grid.hidden = !img;
      if (!img) return;
      const data = analyse(img.data, img.width, img.height, COLUMNS, VECTOR);
      const pixels = img.width * img.height;
      // The graticule's colour, the theme's: read once an update, not once a scope.
      const ink = getComputedStyle(document.documentElement).getPropertyValue("--fg-2").trim() || "gray";
      if (kind === "all" || kind === "waveform") drawTraces(views.waveform.ctx, data, [[200, 255, 210]], pixels, ink);
      if (kind === "all" || kind === "parade") drawTraces(views.parade.ctx, data, [[255, 80, 80], [80, 255, 120], [90, 140, 255]], pixels, ink);
      if (kind === "all" || kind === "vectorscope") drawVector(views.vectorscope.ctx, data, pixels, ink);
      if (kind === "all" || kind === "histogram") drawHistogram(views.histogram.ctx, data, ink);
    });
  };
  // The preview says when it drew: the engine's frame (`frame`), or the grade being dragged
  // (`graded`).
  document.addEventListener("frame", update);
  document.addEventListener("graded", update);
  return { body, tools, show: update };
}

/** Brightness for a count, out of about `full`: logarithmic, so a few pixels show and a flat
 *  colour does not burn. */
const glow = (full: number) => {
  const top = Math.log1p(full);
  return (count: number) => (count ? Math.min(1, 0.25 + Math.log1p(count) / top) : 0);
};

/** Put one opaque pixel of `rgb` at `a` of full brightness into `d` at byte `i`. */
const lit = (d: Uint8ClampedArray, i: number, r: number, g: number, b: number, a: number) => {
  d[i] = r * a;
  d[i + 1] = g * a;
  d[i + 2] = b * a;
  d[i + 3] = 255;
};

/** A waveform (one trace) or a parade (one per channel, side by side): columns across, level up. */
function drawTraces(ctx: CanvasRenderingContext2D, data: ScopeData, colours: number[][], pixels: number, ink: string) {
  const { columns } = data;
  const w = columns * colours.length;
  const img = ctx.createImageData(w, 256);
  // A column's pixels spread over its levels: about a column's worth at one level is full.
  const bright = glow(pixels / columns);
  colours.forEach((rgb, c) => {
    const counts = colours.length === 1 ? data.waveform : data.parade.subarray(c * columns * 256, (c + 1) * columns * 256);
    for (let col = 0; col < columns; col++) {
      for (let level = 0; level < 256; level++) {
        const a = bright(counts[col * 256 + level]);
        if (a) lit(img.data, ((255 - level) * w + c * columns + col) * 4, rgb[0], rgb[1], rgb[2], a);
      }
    }
  });
  ctx.putImageData(img, 0, 0);
  // 0, 25, 50, 75 and 100%.
  ctx.strokeStyle = ink;
  ctx.globalAlpha = 0.35;
  ctx.lineWidth = 1;
  for (const pct of [0, 25, 50, 75, 100]) {
    const y = 255 - Math.round((pct / 100) * 255) + 0.5;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
  }
  ctx.globalAlpha = 1;
}

/** The vectorscope: chroma as a cloud round the centre, the bars' targets and the skin line. */
function drawVector(ctx: CanvasRenderingContext2D, data: ScopeData, pixels: number, ink: string) {
  const n = data.size;
  const img = ctx.createImageData(n, n);
  const bright = glow(pixels / 40);
  for (let i = 0; i < n * n; i++) {
    const a = bright(data.vector[i]);
    if (a) lit(img.data, i * 4, 230, 240, 230, a);
  }
  ctx.putImageData(img, 0, 0);
  const c = (n - 1) / 2;
  ctx.strokeStyle = ink;
  ctx.globalAlpha = 0.45;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.arc(c, c, c, 0, Math.PI * 2);
  ctx.moveTo(0, c);
  ctx.lineTo(n, c);
  ctx.moveTo(c, 0);
  ctx.lineTo(c, n);
  ctx.stroke();
  // The skin line, anticlockwise from +Cb (screen y is down).
  const a = (SKIN_ANGLE * Math.PI) / 180;
  ctx.beginPath();
  ctx.moveTo(c, c);
  ctx.lineTo(c + Math.cos(a) * c, c - Math.sin(a) * c);
  ctx.stroke();
  ctx.font = "8px sans-serif";
  ctx.fillStyle = ctx.strokeStyle;
  for (const t of TARGETS) {
    const [x, y] = vectorAt(t.rgb[0], t.rgb[1], t.rgb[2], n);
    ctx.strokeRect(x - 3, y - 3, 6, 6);
    ctx.fillText(t.name, x + 4, y - 4);
  }
  ctx.globalAlpha = 1;
}

/** The histogram: red, green and blue over each other, and luma. */
function drawHistogram(ctx: CanvasRenderingContext2D, data: ScopeData, ink: string) {
  const { width: w, height: h } = ctx.canvas;
  ctx.clearRect(0, 0, w, h);
  const most = Math.max(1, ...data.histogram);
  const curve = (offset: number, colour: string) => {
    ctx.fillStyle = colour;
    for (let level = 0; level < 256; level++) {
      const v = Math.sqrt(data.histogram[offset + level] / most) * h;
      ctx.fillRect(level, h - v, 1, v);
    }
  };
  ctx.globalCompositeOperation = "lighter";
  curve(0, "rgba(255, 70, 70, 0.7)");
  curve(256, "rgba(70, 255, 110, 0.7)");
  curve(512, "rgba(80, 130, 255, 0.7)");
  ctx.globalCompositeOperation = "source-over";
  ctx.strokeStyle = ink;
  ctx.beginPath();
  for (let level = 0; level < 256; level++) {
    const v = Math.sqrt(data.histogram[768 + level] / most) * h;
    if (level) ctx.lineTo(level + 0.5, h - v);
    else ctx.moveTo(0.5, h - v);
  }
  ctx.stroke();
}
