// Mirrors crates/kuma-core/src/model.rs. Keep in sync (the daemon exposes the JSON
// Schema at /api/schema if you want to check).

export interface Fps { num: number; den: number }

export type MediaKind = "video" | "audio" | "image";

export interface MediaMeta {
  duration?: number; width?: number; height?: number; fps?: Fps; has_audio: boolean;
}

/** A timeline: what a sequence bin entry holds, master included. */
export interface Sequence { tracks: Track[]; markers?: Marker[] }

/** What a bin entry is. Exactly one shape. */
export type BinSource =
  | { path: string; kind: MediaKind; proxy?: string | null }
  | { template: string; vars?: Record<string, unknown> }
  | { color: string }
  | { folder: true }
  | Sequence;

/** One entry in the project bin: media, a configured overlay, a colour, a sequence or a
 *  folder. `parent` is where it is listed in the browser (a transcript under its media);
 *  absent means the top level, and nothing about rendering reads it. */
export interface BinItem { name: string; source: BinSource; meta?: MediaMeta | null; parent?: string; order?: number }

export type TrackKind = "video" | "audio" | "overlay";

export interface Track {
  id: string; kind: TrackKind; name: string; muted: boolean; hidden: boolean;
  clips: Clip[]; transitions: Transition[];
}

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

/** A placement of a bin entry: when it plays, not what it is. */
export interface Clip {
  id: string; start: number; end: number; bin: string; in?: number;
  speed: number; volume: number; muted: boolean; effects: Effect[]; label: string;
}

export interface Transition { from: string; to: string; kind: "dissolve" | "wipe"; duration: number; luma?: string | null; softness?: number }

export interface Marker { t: number; label: string; color?: string | null }

/** There is no project-level `tracks`: every timeline is a sequence entry in the bin, and
 *  `master` names the one that gets rendered. */
export interface Project {
  version: number; name: string; revision: number; fps: Fps; width: number; height: number; sample_rate: number;
  bin: Record<string, BinItem>; master: string;
}

/** What the daemon says about the sequence currently open (session state, not document).
 *  Which sequences exist comes from the project, not from here. */
export interface SequenceState { current: string }

/** One subtitle line, seconds into the source, `end` exclusive. */
export interface Cue { start: number; end: number; text: string }

export interface VarSpec {
  type: "string" | "text" | "number" | "color" | "boolean" | "enum" | "point" | "cues";
  label?: string; description?: string; default: unknown; min?: number; max?: number; step?: number; options?: string[];
}
export interface TemplateManifest {
  id: string; name: string; description: string; duration_default: number; static: boolean;
  vars: Record<string, VarSpec>; tags: string[];
}
export interface TemplateInfo { id: string; manifest: TemplateManifest; has_thumb: boolean }

export interface Preset { id: string; description: string; extension: string }

/** An effect preset: typed params and effects whose values may be `$expr` strings. */
export interface EffectPreset {
  id: string; name: string; description: string;
  params: Record<string, VarSpec>;
  effects: unknown[];
}

/**
 * Expand `$expr` placeholders (param names, numbers, + - * / and parentheses) with the
 * given values (defaults for the rest). `values` may carry extra names such as the
 * project `width`/`height`, which presets use for centring. Inside an `mlt` effect's
 * `properties` the result is a string, as the model requires. A tiny recursive-descent
 * evaluator, so param names like `in` are fine and nothing reaches `eval`.
 */
export function expandPreset(preset: EffectPreset, values: Record<string, number>): Effect[] {
  const vars: Record<string, number> = { ...values };
  for (const [n, spec] of Object.entries(preset.params)) vars[n] ??= Number(spec.default);
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

export interface Job { id: number; kind: string; percent: number; message: string; done: boolean; ok: boolean; result: unknown }

export type DaemonEvent =
  | { type: "project_changed"; revision: number; origin: string }
  | ({ type: "job" } & Job);

export const fpsValue = (f: Fps) => f.num / f.den;

export const isOverlaySource = (s: BinSource): s is { template: string; vars?: Record<string, unknown> } => "template" in s;
export const isFileSource = (s: BinSource): s is { path: string; kind: MediaKind; proxy?: string | null } => "path" in s;
export const isSequenceSource = (s: BinSource): s is Sequence => "tracks" in s;
export const isColorSource = (s: BinSource): s is { color: string } => "color" in s;
export const isFolderSource = (s: BinSource): s is { folder: true } => "folder" in s;

/** What a bin entry is, for badges and track placement. */
export type ItemKind = "overlay" | "video" | "audio" | "image" | "color" | "sequence" | "folder";
export function itemKind(item: BinItem): ItemKind {
  if (isOverlaySource(item.source)) return "overlay";
  if (isSequenceSource(item.source)) return "sequence";
  if (isFolderSource(item.source)) return "folder";
  return isFileSource(item.source) ? item.source.kind : "color";
}

/** The timeline a bin entry holds, if it is a sequence. */
export function sequenceOf(p: Project, id: string): Sequence | undefined {
  const src = p.bin[id]?.source;
  return src && isSequenceSource(src) ? src : undefined;
}

/** Every sequence in the bin, in bin order. */
export function sequenceIds(p: Project): string[] {
  return Object.keys(p.bin).filter((id) => sequenceOf(p, id));
}

/** A sequence's own duration: its latest clip end. */
export function seqDuration(p: Project, id: string): number {
  const seq = sequenceOf(p, id);
  return seq ? Math.max(0, ...seq.tracks.flatMap((t) => t.clips.map((c) => c.end))) : 0;
}

/** Every clip in the project, whichever sequence it sits on. Ids are unique across all of
 *  them, which is what lets anything address a clip by id alone — but this walks the whole
 *  project, so it is for whole-project questions, never a lookup (see `store.findClip`). */
export const allClips = (p: Project): Clip[] => sequenceIds(p).flatMap((id) => sequenceOf(p, id)!.tracks.flatMap((t) => t.clips));

// ---- the parent relation, defined once, the way the sequence relation is below ----

/** The bin grouped by parent, each group in browser order: entries with an `order`
 *  first, by it, then the rest by id (the document's own order). A parent that is not in
 *  the bin (mid-edit) counts as the top level, so nothing is ever lost from the tree. */
export function childrenMap(p: Project): Map<string | null, [string, BinItem][]> {
  const m = new Map<string | null, [string, BinItem][]>();
  for (const [id, item] of Object.entries(p.bin)) {
    const parent = item.parent && item.parent in p.bin ? item.parent : null;
    (m.get(parent) ?? m.set(parent, []).get(parent)!).push([id, item]);
  }
  for (const group of m.values()) group.sort(([, a], [, b]) => (a.order ?? Infinity) - (b.order ?? Infinity));
  return m;
}

/** The parent an entry is listed under, as the tree resolves it. */
export const parentOf = (p: Project, id: string): string | null => {
  const parent = p.bin[id]?.parent;
  return parent && parent in p.bin ? parent : null;
};

/** Entries listed directly under `parent` (`null` for the top level). */
export const childrenOf = (p: Project, parent: string | null): [string, BinItem][] => childrenMap(p).get(parent) ?? [];

/** The entries above `id`, nearest first. Cycle-safe, like `reachableFrom`. */
export function ancestorsOf(p: Project, id: string): Set<string> {
  const seen = new Set<string>();
  for (let at = p.bin[id]?.parent; at && !seen.has(at); at = p.bin[at]?.parent) seen.add(at);
  return seen;
}

/** Would listing `id` under `parent` put it inside itself? The validator refuses the
 *  ring; this is the same rule so the gesture is never offered. */
export const wouldNest = (p: Project, id: string, parent: string | null): boolean =>
  parent !== null && (parent === id || ancestorsOf(p, parent).has(id));

/** How many clips place this bin entry. The delete guard and the "used by N clips" hint
 *  are the same question and must stay the same answer. */
export const placementsOf = (p: Project, binId: string): number => allClips(p).filter((c) => c.bin === binId).length;

/** The sequences reachable from `id`, itself included — what a compile of it would touch,
 *  and therefore the only part of the project that can change its picture. Cycle-safe. */
export function reachableFrom(p: Project, id: string, seen = new Set<string>()): Set<string> {
  const seq = sequenceOf(p, id);
  if (!seq || seen.has(id)) return seen;
  seen.add(id);
  for (const t of seq.tracks) for (const c of t.clips) reachableFrom(p, c.bin, seen);
  return seen;
}

/** Would placing `binId` on sequence `seqId` make a sequence contain itself? The validator
 *  refuses the patch either way; this is so the UI does not offer the gesture. */
export const wouldRecurse = (p: Project, binId: string, seqId: string): boolean => reachableFrom(p, binId).has(seqId);

/** The track an entry goes on when placed, or `null` for one that cannot be placed. One
 *  table, keyed over every kind, so a new kind is a compile error here rather than a
 *  guard somewhere else. */
const TRACK_FOR: Record<ItemKind, TrackKind | null> = { overlay: "overlay", audio: "audio", video: "video", image: "video", color: "video", sequence: "video", folder: null };
export const trackFor = (item: BinItem): TrackKind | null => TRACK_FOR[itemKind(item)];

/** Whether `binId` can go on sequence `seqId` at all: something a track takes, and not a
 *  sequence that would contain itself. */
export const canPlace = (p: Project, binId: string, seqId: string): boolean =>
  !!p.bin[binId] && !!trackFor(p.bin[binId]) && !wouldRecurse(p, binId, seqId);

/** True when a clip placing this entry could be given a speed: MLT cannot time-warp a
 *  timeline. The validator enforces it; this is the same rule for the inspector. */
export const canTimeWarp = (item: BinItem | undefined): boolean => !item || itemKind(item) !== "sequence";

/** JSON pointers into the document. Bin ids are validated to `[A-Za-z_][A-Za-z0-9_-]*` so
 *  the escaping never fires; it is here so every pointer is built one way, and a daemon
 *  that ever loosens that rule cannot silently address a different entry. */
const segment = (id: string) => id.replace(/~/g, "~0").replace(/\//g, "~1");
export const binPath = (binId: string) => `/bin/${segment(binId)}`;
export const seqPath = (seqId: string) => `${binPath(seqId)}/source`;

/** The entry a clip plays, if it resolves. */
export const binItem = (p: Project, c: Clip): BinItem | undefined => p.bin[c.bin];
export const isOverlayClip = (p: Project, c: Clip): boolean => !!binItem(p, c) && itemKind(binItem(p, c)!) === "overlay";
/** True when `in` and a time warp mean something: video and audio only. */
/** True when `in` means something: video, audio and sequences have a timeline to offset
 *  into; a still, a colour and an overlay are generated to fit the clip. */
export const isTimedMedia = (item: BinItem | undefined): boolean =>
  !!item && ["video", "audio", "sequence"].includes(itemKind(item));

/** A media entry whose audio can be transcribed: what the probe said, and the daemon's
 *  own rule when nothing was probed (it tries). */
export const hasAudio = (item: BinItem | undefined): boolean =>
  !!item && isFileSource(item.source) && (item.meta?.has_audio ?? true);

/** Which way round the frame is. Nothing but its size decides it. */
export const orientation = (p: { width: number; height: number }) => (p.height > p.width ? "portrait" : "landscape");

/** The track lane a clip is drawn in. */
export type ClipKind = "overlay" | "video" | "audio" | "color" | "sequence";
export function clipKind(p: Project, c: Clip, track?: Track): ClipKind {
  const item = binItem(p, c);
  if (!item) return "color";
  const k = itemKind(item);
  if (k === "overlay" || k === "color" || k === "audio" || k === "sequence") return k;
  return track?.kind === "audio" ? "audio" : "video";
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
    // `transformAt` may hand back a keyframe object carrying its own `t`: set ours last.
    return { ...e, keyframes: [{ ...transformAt(e, cut), t: 0 }, ...e.keyframes.filter((k) => k.t > cut).map((k) => ({ ...k, t: k.t - cut }))] };
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
    return { ...e, keyframes: [...e.keyframes.filter((k) => k.t <= cut), { ...transformAt(e, cut), t: cut }] };
  }
  if (e.type === "volume" && e.keyframes.some((k) => k.t > cut)) {
    const at = keyframeAt(e.keyframes, cut, (a, b, u) => ({ t: cut, gain: a.gain + (b.gain - a.gain) * u }))!;
    return { ...e, keyframes: [...e.keyframes.filter((k) => k.t <= cut), { t: cut, gain: at.gain }] };
  }
  return e;
}
