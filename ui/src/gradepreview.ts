// The grade, live: while a grade control is dragged, the frame at the playhead as the grade
// being dragged would make it, drawn over the engine's frame — and the before view, the
// frame without that grade, while its button is held.
//
// Both start from the frame without the grade (the clip's effect at its index — a clip may
// have a grade over the whole picture and masked ones besides), which the daemon renders as
// a version of the document (`sample` with `version`), and the grade is the daemon's own
// maths as a table of bytes (`/api/grade/table`), applied here with WebGL — through the
// grade's mask, when it has one, edged as the compiler's is. So the picture is the
// compiler's, not a copy of it kept in the client. It covers the frame — exact for a clip
// that fills it, which is what grading is mostly done on — and goes once the engine shows
// the edit, or the playhead moves.

import { api } from "./api";
import { store } from "./store";
import type { Grade, Mask } from "./types";

/** Points a side of the table: what the render's `.cube` has. */
const SIZE = 33;

/** The frame the grade starts from, by what it is of: clip, moment, revision, sequence. */
let base: { key: string; frame: Promise<ImageBitmap | null> } | null = null;
let view: View | null = null;
/** The grade waiting to be drawn, and whether one is on its way: the latest wins. */
let pending: { clip: string; effect: number; grade: Grade } | null = null;
let drawing = false;
/** Bumped by everything that ends a preview, so a draw that lands after it does not show. */
let generation = 0;

const stage = () => document.querySelector<HTMLElement>("#preview .stage");

/** Show `grade` as `clip`'s effect at `effect`, at the playhead, as soon as the frame and
 *  its table are here. */
export function previewGrade(clip: string, effect: number, grade: Grade): void {
  pending = { clip, effect, grade };
  void pump();
}

/** Show the frame without `clip`'s effect at `effect` (`on`), or stop. */
export function showBefore(clip: string, effect: number, on: boolean): void {
  if (!on) return hide();
  const g = ++generation;
  void frameFor(clip, effect).then((frame) => { if (frame && g === generation) paint(frame, null, undefined); });
}

/** An edit was made from the preview: keep it until the engine shows the edit, then go. */
export function settle(): void {
  const g = generation;
  const st = stage();
  const done = () => { if (g === generation) hide(); };
  // The engine draws a frame for the edit; if it never does (no engine, nothing changed),
  // the snapshot path or the timeout clears it.
  st?.addEventListener("frame", done, { once: true });
  window.setTimeout(done, 3000);
}

function hide(): void {
  generation++;
  pending = null;
  if (view && !view.canvas.hidden) {
    view.canvas.hidden = true;
    // The frame shows the engine's picture again: what reads it reads it again.
    view.canvas.dispatchEvent(new Event("graded", { bubbles: true }));
  }
}

async function pump(): Promise<void> {
  if (drawing) return;
  drawing = true;
  try {
    while (pending) {
      const { clip, effect, grade } = pending;
      pending = null;
      const g = generation;
      // The table is of the colour alone: the mask is drawn here, not baked into it.
      const { mask, ...colour } = grade;
      const [frame, table] = await Promise.all([frameFor(clip, effect), api.gradeTable(colour, SIZE).catch(() => null)]);
      if (frame && table && g === generation && !pending) paint(frame, table, mask);
    }
  } finally {
    drawing = false;
  }
}

/** The frame at the playhead without `clip`'s effect at `effect`, rendered once per clip,
 *  effect, moment and revision. */
function frameFor(clip: string, effect: number): Promise<ImageBitmap | null> {
  const p = store.state.project;
  const st = stage();
  if (!p || !st) return Promise.resolve(null);
  const t = store.state.playhead;
  const key = `${clip}#${effect}@${t}@${p.revision}@${store.state.sequence}`;
  if (base?.key === key) return base.frame;
  const width = Math.ceil(st.clientWidth * (window.devicePixelRatio || 1));
  const frame = api.sample({
    t0: t, times: [t], width, overlays: false, sequence: store.state.sequence,
    version: { without: { clips: [clip], effects: [effect] } },
  }).then(async (r) => createImageBitmap(await (await fetch(api.sampleFile(r.samples[0].path))).blob())).catch(() => null);
  base = { key, frame };
  return frame;
}

const UNIFORMS = ["useLut", "shape", "size", "centre", "radii", "turn", "feather", "invert"] as const;

interface View {
  canvas: HTMLCanvasElement;
  gl: WebGL2RenderingContext;
  frame: WebGLTexture;
  lut: WebGLTexture;
  uniforms: Record<(typeof UNIFORMS)[number], WebGLUniformLocation>;
  /** The frame in `frame` now: a drag changes the table, not the frame, so it goes up once. */
  shown: ImageBitmap | null;
}

const VERTEX = `#version 300 es
in vec2 at;
out vec2 uv;
// The frame's first row is its top, and clip space's top is +1.
void main() { uv = vec2(at.x * 0.5 + 0.5, 0.5 - at.y * 0.5); gl_Position = vec4(at, 0.0, 1.0); }`;

// The table's points sit at the centres of its texels: a colour c reads at c·(n−1)/n + ½/n.
// A mask (`shape` 1 an ellipse, 2 a rectangle; 0 none) is in composition pixels, turned
// clockwise by `turn` about its centre, its edge `feather` wide and centred on the shape's
// edge — the compiler's `mask_start`, by distance rather than by blurring a matte.
const FRAGMENT = `#version 300 es
precision highp float;
precision highp sampler3D;
in vec2 uv;
out vec4 color;
uniform sampler2D frame;
uniform sampler3D lut;
uniform bool useLut;
uniform int shape;
uniform vec2 size, centre, radii;
uniform float turn, feather;
uniform bool invert;
void main() {
  vec3 c = texture(frame, uv).rgb;
  float n = ${SIZE}.0;
  vec3 graded = useLut ? texture(lut, c * (n - 1.0) / n + 0.5 / n).rgb : c;
  float inside = 1.0;
  if (shape != 0) {
    vec2 d = uv * size - centre;
    vec2 q = vec2(d.x * cos(turn) + d.y * sin(turn), -d.x * sin(turn) + d.y * cos(turn));
    float edge = shape == 1 ? (length(q / radii) - 1.0) * min(radii.x, radii.y) : max(abs(q.x) - radii.x, abs(q.y) - radii.y);
    inside = feather > 0.0 ? 1.0 - smoothstep(-feather / 2.0, feather / 2.0, edge) : step(edge, 0.0);
    if (invert) inside = 1.0 - inside;
  }
  color = vec4(mix(c, graded, inside), 1.0);
}`;

/** The canvas, made once, between the engine's frame and the live overlays. */
function ensureView(): View | null {
  const st = stage();
  if (!st) return null;
  if (view && view.canvas.isConnected) return view;
  const canvas = document.createElement("canvas");
  canvas.className = "grade-live";
  canvas.hidden = true;
  st.insertBefore(canvas, st.querySelector(".overlay-layer"));
  // Kept after it is shown, so the scopes can read what the grade being dragged looks like.
  const gl = canvas.getContext("webgl2", { premultipliedAlpha: false, preserveDrawingBuffer: true });
  if (!gl) {
    canvas.remove();
    return null;
  }
  const shader = (type: number, src: string) => {
    const s = gl.createShader(type)!;
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) console.error("grade preview shader:", gl.getShaderInfoLog(s));
    return s;
  };
  const program = gl.createProgram()!;
  gl.attachShader(program, shader(gl.VERTEX_SHADER, VERTEX));
  gl.attachShader(program, shader(gl.FRAGMENT_SHADER, FRAGMENT));
  gl.linkProgram(program);
  // A program that does not link draws black: no preview is better than a wrong one.
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    canvas.remove();
    return null;
  }
  gl.useProgram(program);
  const quad = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, quad);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
  const at = gl.getAttribLocation(program, "at");
  gl.enableVertexAttribArray(at);
  gl.vertexAttribPointer(at, 2, gl.FLOAT, false, 0, 0);
  const texture = (unit: number, target: number) => {
    const t = gl.createTexture()!;
    gl.activeTexture(gl.TEXTURE0 + unit);
    gl.bindTexture(target, t);
    for (const p of [gl.TEXTURE_WRAP_S, gl.TEXTURE_WRAP_T, gl.TEXTURE_WRAP_R]) gl.texParameteri(target, p, gl.CLAMP_TO_EDGE);
    gl.texParameteri(target, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(target, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    return t;
  };
  const frame = texture(0, gl.TEXTURE_2D);
  const lut = texture(1, gl.TEXTURE_3D);
  gl.uniform1i(gl.getUniformLocation(program, "frame"), 0);
  gl.uniform1i(gl.getUniformLocation(program, "lut"), 1);
  const uniforms = Object.fromEntries(UNIFORMS.map((n) => [n, gl.getUniformLocation(program, n)!])) as View["uniforms"];
  view = { canvas, gl, frame, lut, uniforms, shown: null };
  return view;
}

/** Draw `frame` through `table` (a SIZE³ RGB table) where `mask` lets it, or as it is when
 *  there is no table. */
function paint(frame: ImageBitmap, table: Uint8Array | null, mask: Mask | undefined): void {
  const v = ensureView();
  if (!v) return;
  const { gl, canvas } = v;
  if (canvas.width !== frame.width || canvas.height !== frame.height) {
    canvas.width = frame.width;
    canvas.height = frame.height;
  }
  gl.viewport(0, 0, canvas.width, canvas.height);
  if (v.shown !== frame) {
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, v.frame);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, frame);
    v.shown = frame;
  }
  if (table) {
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_3D, v.lut);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texImage3D(gl.TEXTURE_3D, 0, gl.RGB8, SIZE, SIZE, SIZE, 0, gl.RGB, gl.UNSIGNED_BYTE, table);
  }
  const u = v.uniforms;
  gl.uniform1i(u.useLut, table ? 1 : 0);
  const p = store.state.project;
  gl.uniform1i(u.shape, mask && p ? (mask.shape === "rectangle" ? 2 : 1) : 0);
  if (mask && p) {
    gl.uniform2f(u.size, p.width, p.height);
    gl.uniform2f(u.centre, mask.x + mask.w / 2, mask.y + mask.h / 2);
    gl.uniform2f(u.radii, Math.max(mask.w / 2, 0.5), Math.max(mask.h / 2, 0.5));
    gl.uniform1f(u.turn, ((mask.rotation ?? 0) * Math.PI) / 180);
    gl.uniform1f(u.feather, mask.feather ?? 0);
    gl.uniform1i(u.invert, mask.invert ? 1 : 0);
  }
  gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  canvas.hidden = false;
  canvas.dispatchEvent(new Event("graded", { bubbles: true }));
}

// A preview is of one moment: moving, playing or picking something else ends it.
store.select((s) => [s.playhead, s.playing, s.selection, s.sequence], () => { if (view && !view.canvas.hidden) hide(); });
