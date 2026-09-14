// Mirrors crates/kuma-core/src/model.rs. Keep in sync (the daemon exposes the JSON
// Schema at /api/schema if you want to check).

export interface Fps { num: number; den: number }

export interface AssetMeta {
  duration?: number; width?: number; height?: number; fps?: Fps; has_audio: boolean; codec?: string;
}
export interface Asset { path: string; kind: "video" | "audio" | "image"; proxy?: string | null; meta?: AssetMeta | null }

export type TrackKind = "video" | "audio" | "overlay";

export interface Track {
  id: string; kind: TrackKind; name: string; muted: boolean; hidden: boolean;
  clips: Clip[]; transitions: Transition[];
}

export type ClipSource =
  | { asset: string; in?: number }
  | { overlay: string; vars?: Record<string, unknown> }
  | { color: string };

export interface TransformKeyframe { t: number; x?: number; y?: number; scale?: number; rotation?: number; opacity?: number }

export type Effect =
  | { type: "transform"; x?: number; y?: number; scale?: number; rotation?: number; opacity?: number; keyframes?: TransformKeyframe[] }
  | { type: "crop"; left?: number; right?: number; top?: number; bottom?: number; radius?: number }
  | { type: "blend"; mode: string }
  | { type: "blur"; radius: number }
  | { type: "color"; brightness?: number; contrast?: number; saturation?: number; gamma?: number }
  | { type: "fade"; in?: number; out?: number }
  | { type: "volume"; keyframes: { t: number; gain: number }[] }
  | { type: "mlt"; service: string; properties?: Record<string, string> };

export interface Clip {
  id: string; start: number; end: number; src: ClipSource;
  speed: number; volume: number; muted: boolean; effects: Effect[]; label: string;
}

export interface Transition { from: string; to: string; kind: "dissolve" | "wipe"; duration: number; luma?: string | null; softness?: number }

export interface Project {
  version: number; name: string; revision: number; fps: Fps; width: number; height: number; sample_rate: number;
  assets: Record<string, Asset>; tracks: Track[]; markers: { t: number; label: string; color?: string | null }[];
}

export interface VarSpec {
  type: "string" | "text" | "number" | "color" | "boolean" | "enum" | "asset" | "point";
  label?: string; description?: string; default: unknown; min?: number; max?: number; step?: number; options?: string[];
}
export interface TemplateManifest {
  id: string; name: string; description: string; duration_default: number; entry: string; static: boolean;
  region?: { x: number; y: number; w: number; h: number }; vars: Record<string, VarSpec>; tags: string[];
}
export interface TemplateInfo { id: string; dir: string; manifest: TemplateManifest; has_thumb: boolean }

export interface Preset { id: string; description: string; extension: string }

/** An effect preset: typed params and effects whose values may be `$expr` strings. */
export interface EffectPreset {
  id: string; name: string; description: string;
  params: Record<string, VarSpec>;
  effects: unknown[];
}

/**
 * Expand `$expr` placeholders (param names, numbers, + - * / and parentheses) with the
 * given values (defaults for the rest). Inside an `mlt` effect's `properties` the result
 * is a string, as the model requires. A tiny recursive-descent evaluator, so param
 * names like `in` are fine and nothing reaches `eval`.
 */
export function expandPreset(preset: EffectPreset, values: Record<string, number>): Effect[] {
  const vars: Record<string, number> = {};
  for (const [n, spec] of Object.entries(preset.params)) vars[n] = values[n] ?? Number(spec.default);
  const walk = (v: unknown, asString: boolean): unknown => {
    if (typeof v === "string" && v.startsWith("$")) { const n = evalArith(v.slice(1), vars); return asString ? String(n) : n; }
    if (Array.isArray(v)) return v.map((x) => walk(x, asString));
    if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, walk(x, asString || k === "properties")]));
    return v;
  };
  return walk(preset.effects, false) as Effect[];
}

/** `expr` over `vars`: numbers, identifiers, unary minus, + - * / and parentheses. */
export function evalArith(expr: string, vars: Record<string, number>): number {
  const tokens = expr.match(/\d+(?:\.\d+)?(?:e[+-]?\d+)?|[A-Za-z_]\w*|[-+*/()]/gi) ?? [];
  if (tokens.join("") !== expr.replace(/\s+/g, "")) throw new Error(`bad expression: ${expr}`);
  let i = 0;
  const peek = () => tokens[i];
  const next = () => tokens[i++];
  const atom = (): number => {
    const t = next();
    if (t === undefined) throw new Error(`bad expression: ${expr}`);
    if (t === "(") { const v = sum(); if (next() !== ")") throw new Error(`bad expression: ${expr}`); return v; }
    if (t === "-") return -atom();
    if (t === "+") return atom();
    if (/^[A-Za-z_]/.test(t)) { if (!(t in vars)) throw new Error(`unknown param \`${t}\` in: ${expr}`); return vars[t]; }
    return Number(t);
  };
  const product = (): number => { let v = atom(); while (peek() === "*" || peek() === "/") v = next() === "*" ? v * atom() : v / atom(); return v; };
  const sum = (): number => { let v = product(); while (peek() === "+" || peek() === "-") v = next() === "+" ? v + product() : v - product(); return v; };
  const v = sum();
  if (i !== tokens.length || !Number.isFinite(v)) throw new Error(`bad expression: ${expr}`);
  return v;
}

export interface Issue { severity: "error" | "warning"; path: string; message: string }

export type PatchOp =
  | { op: "add"; path: string; value: unknown }
  | { op: "replace"; path: string; value: unknown }
  | { op: "remove"; path: string }
  | { op: "move"; from: string; path: string };

export type DaemonEvent =
  | { type: "project_changed"; revision: number; origin: string }
  | { type: "job_progress"; id: number; kind: string; percent: number; message: string }
  | { type: "job_done"; id: number; kind: string; ok: boolean; message: string; result: unknown };

export const fpsValue = (f: Fps) => f.num / f.den;
export const isOverlay = (c: Clip): c is Clip & { src: { overlay: string; vars?: Record<string, unknown> } } => "overlay" in c.src;
export const isAsset = (c: Clip): c is Clip & { src: { asset: string; in?: number } } => "asset" in c.src;

/** What a clip is for display and track placement. */
export type ClipKind = "overlay" | "video" | "audio" | "color";
export function clipKind(p: Project, c: Clip, track?: Track): ClipKind {
  if (isOverlay(c)) return "overlay";
  if (isAsset(c)) return p.assets[c.src.asset]?.kind === "audio" || track?.kind === "audio" ? "audio" : "video";
  return "color";
}

/** Lowest free `${prefix}${n}` (n ≥ 1) given the ids already in use. */
export function freeId(prefix: string, taken: Iterable<string>): string {
  const ids = new Set(taken);
  let n = 1;
  while (ids.has(`${prefix}${n}`)) n++;
  return `${prefix}${n}`;
}

/** Linear interpolation of a keyframe list at time `t` (relative to the clip start). */
export function keyframeAt<K extends { t: number }>(kfs: K[], t: number, mix: (a: K, b: K, u: number) => K): K | undefined {
  if (!kfs.length) return undefined;
  if (t <= kfs[0].t) return kfs[0];
  for (let i = 1; i < kfs.length; i++) {
    if (t <= kfs[i].t) return mix(kfs[i - 1], kfs[i], (t - kfs[i - 1].t) / Math.max(1e-6, kfs[i].t - kfs[i - 1].t));
  }
  return kfs[kfs.length - 1];
}

export type TransformSample = { x: number; y: number; scale: number; rotation: number; opacity: number };

/** The transform effect's values at `local` seconds, keyframes applied. */
export function transformAt(e: Extract<Effect, { type: "transform" }>, local: number): TransformSample {
  const base: TransformSample = { x: e.x ?? 0, y: e.y ?? 0, scale: e.scale ?? 1, rotation: e.rotation ?? 0, opacity: e.opacity ?? 1 };
  const fill = (k: TransformKeyframe): TransformSample & { t: number } => ({ t: k.t, x: k.x ?? base.x, y: k.y ?? base.y, scale: k.scale ?? base.scale, rotation: k.rotation ?? base.rotation, opacity: k.opacity ?? base.opacity });
  const lerp = (a: number, b: number, u: number) => a + (b - a) * u;
  const s = keyframeAt((e.keyframes ?? []).map(fill), local, (a, b, u) => ({ t: local, x: lerp(a.x, b.x, u), y: lerp(a.y, b.y, u), scale: lerp(a.scale, b.scale, u), rotation: lerp(a.rotation, b.rotation, u), opacity: lerp(a.opacity, b.opacity, u) }));
  return s ?? base;
}

/** Re-base a time-relative effect for a clip that starts `cut` seconds later (the right
 *  half of a split): keyframes shift, the value at the cut becomes the first keyframe. */
export function shiftEffect(e: Effect, cut: number): Effect {
  if (e.type === "fade") return { ...e, in: 0 };
  if (e.type === "transform" && e.keyframes?.length) {
    const at = transformAt(e, cut);
    return { ...e, keyframes: [{ t: 0, ...at }, ...e.keyframes.filter((k) => k.t > cut).map((k) => ({ ...k, t: k.t - cut }))] };
  }
  if (e.type === "volume") {
    const at = keyframeAt(e.keyframes, cut, (a, b, u) => ({ t: cut, gain: a.gain + (b.gain - a.gain) * u }))?.gain ?? 1;
    return { ...e, keyframes: [{ t: 0, gain: at }, ...e.keyframes.filter((k) => k.t > cut).map((k) => ({ ...k, t: k.t - cut }))] };
  }
  return e;
}

/** The complementary trim for the left half: keep keyframes up to the cut, then hold. */
export function trimEffect(e: Effect, cut: number): Effect {
  if (e.type === "fade") return { ...e, out: 0 };
  if (e.type === "transform" && e.keyframes?.some((k) => k.t > cut)) {
    return { ...e, keyframes: [...e.keyframes.filter((k) => k.t <= cut), { t: cut, ...transformAt(e, cut) }] };
  }
  if (e.type === "volume" && e.keyframes.some((k) => k.t > cut)) {
    const at = keyframeAt(e.keyframes, cut, (a, b, u) => ({ t: cut, gain: a.gain + (b.gain - a.gain) * u }))!;
    return { ...e, keyframes: [...e.keyframes.filter((k) => k.t <= cut), { t: cut, gain: at.gain }] };
  }
  return e;
}
