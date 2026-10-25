// The grade, live: while a grade control is dragged, the frame at the playhead as the grade
// being dragged would make it, drawn over the engine's frame — and the before view, the
// frame without the clip's grade, while its button is held.
//
// Both start from the frame without the clip's grade, which the daemon renders as a
// version of the document (`sample` with `version`), and the grade is the daemon's own maths
// as a table of bytes (`/api/grade/table`), applied here with WebGL. So the picture is the
// compiler's, not a copy of it kept in the client. It covers the frame — exact for a clip
// that fills it, which is what grading is mostly done on — and goes once the engine shows
// the edit, or the playhead moves.

import { api } from "./api";
import { store } from "./store";
import type { Grade } from "./types";

/** Points a side of the table: what the render's `.cube` has. */
const SIZE = 33;

/** The frame the grade starts from, by what it is of: clip, moment, revision, sequence. */
let base: { key: string; frame: Promise<ImageBitmap | null> } | null = null;
let view: View | null = null;
/** The grade waiting to be drawn, and whether one is on its way: the latest wins. */
let pending: { clip: string; grade: Grade } | null = null;
let drawing = false;
/** Bumped by everything that ends a preview, so a draw that lands after it does not show. */
let generation = 0;

const stage = () => document.querySelector<HTMLElement>("#preview .stage");

/** Show `grade` on `clip` at the playhead, as soon as the frame and its table are here. */
export function previewGrade(clip: string, grade: Grade): void {
  pending = { clip, grade };
  void pump();
}

/** Show the frame without `clip`'s grade (`on`), or stop. */
export function showBefore(clip: string, on: boolean): void {
  if (!on) return hide();
  const g = ++generation;
  void frameFor(clip).then((frame) => { if (frame && g === generation) paint(frame, null); });
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
  if (view) view.canvas.hidden = true;
}

async function pump(): Promise<void> {
  if (drawing) return;
  drawing = true;
  try {
    while (pending) {
      const { clip, grade } = pending;
      pending = null;
      const g = generation;
      const [frame, table] = await Promise.all([frameFor(clip), api.gradeTable(grade, SIZE).catch(() => null)]);
      if (frame && table && g === generation && !pending) paint(frame, table);
    }
  } finally {
    drawing = false;
  }
}

/** The frame at the playhead without `clip`'s grade, rendered once per clip, moment and revision. */
function frameFor(clip: string): Promise<ImageBitmap | null> {
  const p = store.state.project;
  const st = stage();
  if (!p || !st) return Promise.resolve(null);
  const t = store.state.playhead;
  const key = `${clip}@${t}@${p.revision}@${store.state.sequence}`;
  if (base?.key === key) return base.frame;
  const width = Math.ceil(st.clientWidth * (window.devicePixelRatio || 1));
  const frame = api.sample({
    t0: t, times: [t], width, overlays: false, sequence: store.state.sequence,
    version: { without: { kinds: ["grade"], clips: [clip] } },
  }).then(async (r) => createImageBitmap(await (await fetch(api.sampleFile(r.samples[0].path))).blob())).catch(() => null);
  base = { key, frame };
  return frame;
}

interface View {
  canvas: HTMLCanvasElement;
  gl: WebGL2RenderingContext;
  frame: WebGLTexture;
  lut: WebGLTexture;
  useLut: WebGLUniformLocation;
}

const VERTEX = `#version 300 es
in vec2 at;
out vec2 uv;
void main() { uv = vec2(at.x, 1.0 - at.y) * 0.5 + 0.5; gl_Position = vec4(at, 0.0, 1.0); }`;

// The table's points sit at the centres of its texels: a colour c reads at c·(n−1)/n + ½/n.
const FRAGMENT = `#version 300 es
precision highp float;
precision highp sampler3D;
in vec2 uv;
out vec4 color;
uniform sampler2D frame;
uniform sampler3D lut;
uniform bool useLut;
void main() {
  vec3 c = texture(frame, uv).rgb;
  float n = ${SIZE}.0;
  color = vec4(useLut ? texture(lut, c * (n - 1.0) / n + 0.5 / n).rgb : c, 1.0);
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
  const gl = canvas.getContext("webgl2", { premultipliedAlpha: false });
  if (!gl) return null;
  const shader = (type: number, src: string) => {
    const s = gl.createShader(type)!;
    gl.shaderSource(s, src);
    gl.compileShader(s);
    return s;
  };
  const program = gl.createProgram()!;
  gl.attachShader(program, shader(gl.VERTEX_SHADER, VERTEX));
  gl.attachShader(program, shader(gl.FRAGMENT_SHADER, FRAGMENT));
  gl.linkProgram(program);
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
  view = { canvas, gl, frame, lut, useLut: gl.getUniformLocation(program, "useLut")! };
  return view;
}

/** Draw `frame` through `table` (a SIZE³ RGB table), or as it is when there is none. */
function paint(frame: ImageBitmap, table: Uint8Array | null): void {
  const v = ensureView();
  if (!v) return;
  const { gl, canvas } = v;
  if (canvas.width !== frame.width || canvas.height !== frame.height) {
    canvas.width = frame.width;
    canvas.height = frame.height;
  }
  gl.viewport(0, 0, canvas.width, canvas.height);
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, v.frame);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, frame);
  if (table) {
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_3D, v.lut);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texImage3D(gl.TEXTURE_3D, 0, gl.RGB8, SIZE, SIZE, SIZE, 0, gl.RGB, gl.UNSIGNED_BYTE, table);
  }
  gl.uniform1i(v.useLut, table ? 1 : 0);
  gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  canvas.hidden = false;
}

// A preview is of one moment: moving, playing or picking something else ends it.
store.select((s) => [s.playhead, s.playing, s.selection, s.sequence], () => { if (view && !view.canvas.hidden) hide(); });
