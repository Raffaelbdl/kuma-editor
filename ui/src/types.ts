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

/** Expand `$expr` placeholders (param names, numbers, + - * / parentheses) with the given values. */
export function expandPreset(preset: EffectPreset, values: Record<string, number>): Effect[] {
  const names = Object.keys(preset.params);
  const bad = names.find((n) => !/^[A-Za-z_]\w*$/.test(n));
  if (bad) throw new Error(`preset param \`${bad}\` is not an identifier`);
  const args = names.map((n) => values[n] ?? Number(preset.params[n].default));
  const evalExpr = (expr: string): number => {
    // Only arithmetic over the declared params reaches `Function`; no `.`, so no member access.
    const unknown = (expr.match(/[A-Za-z_]\w*/g) ?? []).find((id) => !names.includes(id));
    if (unknown || !/^[\w\s+\-*/().]*$/.test(expr)) throw new Error(`bad expression: ${expr}`);
    // eslint-disable-next-line no-new-func
    return Number(new Function(...names, `return (${expr});`)(...args));
  };
  const walk = (v: unknown): unknown => {
    if (typeof v === "string" && v.startsWith("$")) return evalExpr(v.slice(1));
    if (Array.isArray(v)) return v.map(walk);
    if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, walk(x)]));
    return v;
  };
  return walk(preset.effects) as Effect[];
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
