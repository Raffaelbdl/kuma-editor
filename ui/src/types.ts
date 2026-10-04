// Mirrors crates/kuma-core/src/model.rs. Keep in sync (the daemon exposes the JSON
// Schema at /api/schema if you want to check).

export interface Fps { num: number; den: number }

export type MediaKind = "video" | "audio" | "image";

export interface MediaMeta {
  duration?: number; width?: number; height?: number; fps?: Fps; variable_fps?: boolean; has_audio: boolean;
}

/** A timeline: what a sequence bin entry holds, master included. */
export interface Sequence { tracks: Track[]; markers?: Marker[] }

/** An overlay entry: a project folder (`overlays/<name>`) and its vars. */
export interface OverlaySource { overlay: string; vars?: Record<string, unknown> }

/** What a bin entry is. Exactly one shape. */
export type BinSource =
  | { path: string; kind: MediaKind; proxy?: string | null }
  | OverlaySource
  | { color: string }
  | { folder: true }
  | Sequence;

/** One entry in the project bin: media, a configured overlay, a colour, a sequence or a
 *  folder. `parent` is where it is listed in the browser (a transcript under its media);
 *  absent means the top level, and nothing about rendering reads it. */
export interface BinItem { name: string; source: BinSource; meta?: MediaMeta | null; parent?: string; order?: number; volume?: number }

export type TrackKind = "video" | "audio" | "overlay";

export interface Track {
  id: string; kind: TrackKind; name: string; muted: boolean; hidden: boolean;
  clips: Clip[]; transitions: Transition[];
}

/** How a keyframe reaches the **next** one, MLT's rule: the key that opens a segment
 *  decides that segment's shape, so the last key's ease never means anything. */
export type Ease = "linear" | "hold" | "smooth";

/** One keyframe of any effect: when, how it reaches the next one, and whichever of the
 *  effect's numeric parameters it sets, by the names the static fields have. A parameter
 *  a key leaves out means the effect's static value — the same rule as `compile.rs`. */
export type Keyframe = { t: number; ease?: Ease } & { [param: string]: number | Ease | undefined };

/**
 * What each effect can animate, and what a parameter means when the effect does not state
 * it (its own default). Mirrors `Effect::animatable` in `model.rs`; a kind absent here
 * takes no keyframes — a fade's numbers are lengths of time, a blend mode is a choice, and
 * `mlt` properties are strings MLT reads.
 */
export const ANIMATABLE: Partial<Record<Effect["type"], Record<string, number>>> = {
  transform: { x: 0, y: 0, scale: 1, rotation: 0, opacity: 1 },
  crop: { left: 0, right: 0, top: 0, bottom: 0, radius: 0 },
  blur: { radius: 0 },
  color: { brightness: 0, contrast: 1, saturation: 1, gamma: 1 },
  volume: { gain: 1 },
};

/** What a clip can show and sound — what an effect has to act on to do anything. */
export interface Streams { video: boolean; audio: boolean }

/** What each effect acts on. Mirrors `Effect::streams` in `model.rs`: `mlt`'s service is
 *  opaque, so it is both. A record over every kind, so a new one is a compile error here. */
const EFFECT_STREAMS: Record<Effect["type"], Streams> = {
  transform: { video: true, audio: false },
  crop: { video: true, audio: false },
  blend: { video: true, audio: false },
  blur: { video: true, audio: false },
  color: { video: true, audio: false },
  fade: { video: true, audio: false },
  audio_fade: { video: false, audio: true },
  volume: { video: false, audio: true },
  mlt: { video: true, audio: true },
};

/** What an effect of this kind acts on. */
export const streamsOf = (type: Effect["type"]): Streams => EFFECT_STREAMS[type];

/** Whether an effect of this kind does anything to a clip carrying `clip`. */
export function actsOn(type: Effect["type"], clip: Streams): boolean {
  const s = streamsOf(type);
  return (s.video && clip.video) || (s.audio && clip.audio);
}

/** A fade: two lengths of time, at the clip's start and its end, whichever stream it fades.
 *  What a split does to one is the same for both. */
const isFade = (e: Effect): e is Extract<Effect, { type: "fade" | "audio_fade" }> => e.type === "fade" || e.type === "audio_fade";

/** What a clip on `track` can show and sound. Mirrors `Project::clip_streams` in
 *  `model.rs`: a capability, not a state, so a mute changes nothing; picture is the
 *  track's to allow and the source's to have, sound the source's alone. */
export function clipStreams(p: Project, track: Track, clip: Clip): Streams {
  const item = binItem(p, clip);
  if (!item) return { video: false, audio: false };
  const kind = itemKind(item);
  const video = kind !== "audio" && kind !== "folder";
  const audio = kind === "sequence" ? sequenceSounds(p, clip.bin) : isTimedMedia(item) && hasAudio(item);
  return { video: video && track.kind !== "audio", audio };
}

/** Whether a sequence carries any sound, following nested ones; the inner mutes are its
 *  content, so they count. Mirrors `Project::seq_has_audio`, and walks as `reachableFrom`
 *  does, less what is muted. Cycle-safe. */
function sequenceSounds(p: Project, id: string, seen = new Set<string>()): boolean {
  const seq = sequenceOf(p, id);
  if (!seq || seen.has(id)) return false;
  seen.add(id);
  return seq.tracks.some((t) => !t.muted && t.clips.some((c) => {
    if (c.muted) return false;
    const item = binItem(p, c);
    return !!item && (itemKind(item) === "sequence" ? sequenceSounds(p, c.bin, seen) : isTimedMedia(item));
  }));
}

export type Effect =
  | { type: "transform"; x?: number; y?: number; scale?: number; rotation?: number; opacity?: number; keyframes?: Keyframe[] }
  | { type: "crop"; left?: number; right?: number; top?: number; bottom?: number; radius?: number; keyframes?: Keyframe[] }
  | { type: "blend"; mode: string }
  | { type: "blur"; radius: number; keyframes?: Keyframe[] }
  | { type: "color"; brightness?: number; contrast?: number; saturation?: number; gamma?: number; keyframes?: Keyframe[] }
  | { type: "fade"; in?: number; out?: number }
  | { type: "audio_fade"; in?: number; out?: number }
  | { type: "volume"; gain?: number; keyframes?: Keyframe[] }
  | { type: "mlt"; service: string; properties?: Record<string, string> };

/** A placement of a bin entry: when it plays, not what it is. */
export interface Clip {
  id: string; start: number; end: number; bin: string; in?: number;
  speed: number; volume: number; muted: boolean; effects: Effect[]; label: string;
  /** The group this clip belongs to, or absent. Clips sharing a group are selected
   *  together and move together; the compiler never reads it. See `model.rs`. */
  group?: string;
}

export interface Transition { from: string; to: string; kind: "dissolve" | "wipe"; duration: number; luma?: string | null; softness?: number }
/** The wipe patterns the compiler draws itself — `kuma_core::LUMAS`. A `luma` may also be a
 *  PGM/PNG under the project, which the inspector keeps but does not offer. */
export const LUMAS = ["wipe_left", "wipe_right", "wipe_up", "wipe_down", "iris", "diagonal"];

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
  type: "string" | "text" | "number" | "color" | "boolean" | "enum" | "point" | "cues" | "font" | "lang";
  label?: string; description?: string; default: unknown; min?: number; max?: number; step?: number; options?: string[];
  /** The inspector section it is listed under. */
  group?: string;
  /** Shown only while each named var has this value (or one of these values). */
  when?: Record<string, unknown>;
}
/** `overlay.json`: the same shape for a project's overlay and a library blueprint. Vars
 *  arrive in the order the manifest writes them, which is the order they are shown in. */
export interface OverlayManifest {
  name: string; description: string; duration_default: number; static: boolean;
  vars: Record<string, VarSpec>; tags: string[];
  /** Offered in the bin's New menu under this label. */
  create?: string;
}
/** A font family a `font` var can name, from the project's `fonts/` or a library's. */
export interface FontFamily {
  name: string;
  origin: "project" | "library";
  /** Writing systems it can set: text in any other falls back to the machine's fonts. */
  scripts: string[];
  faces: { weight: [number, number]; italic: boolean; scripts: string[] }[];
}
/** A reusable overlay in the user's library. Copied into a project to be used. */
export interface BlueprintInfo { id: string; manifest: OverlayManifest }
/** An overlay folder the bin uses: its manifest, or why it will not load. */
export interface OverlayInfo { manifest?: OverlayManifest; error?: string }

/** What `check` says about an overlay: the contract's own rules plus hyperframes'. */
export interface Finding { level: "error" | "warning"; code: string; message: string; fix?: string }
export interface LintReport { overlay: string; findings: Finding[]; renderer_skipped?: string }

/** An export preset, and what a render may change of it: the codec's rate factor (lower
 *  is better and larger) and its speed, each with the preset's own value. */
export interface Preset {
  id: string; description: string; extension: string;
  quality: { key: string; default: number; min: number; max: number } | null;
  /** `options` fastest first. */
  speed: { key: string; label: string; default: string; options: string[] } | null;
  /** The hardware it needs, if any, and why the daemon's machine cannot run it, if not. */
  needs?: string | null;
  unavailable?: string | null;
}

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

export type JobState = "queued" | "running" | "done" | "failed" | "cancelled";
/** Done, failed or cancelled: it will not change again. */
export const isFinal = (j: Job) => j.state !== "queued" && j.state !== "running";

export interface Job {
  id: number; kind: string; percent: number; message: string; result: unknown;
  /** Where it is in its life — the field to read; `message` is only a label. */
  state: JobState;
  /** Whether it can be stopped (renders can). */
  cancellable?: boolean;
  /** What it was asked to do, settled when it started — a render's `{sequence, preset, output}`. */
  params?: unknown;
}

/** One part of the project's `.kuma/` cache: entries (a bake is one) and their bytes. */
export interface CachePart { entries: number; bytes: number }
/** How big the cache is, by part. */
export interface CacheUsage { bakes: CachePart; samples: CachePart; thumbs: CachePart; bytes: number }

export type DaemonEvent =
  | { type: "project_changed"; revision: number; origin: string }
  | ({ type: "job" } & Job)
  | { type: "jobs_cleared"; ids: number[] };

export const fpsValue = (f: Fps) => f.num / f.den;

export const isOverlaySource = (s: BinSource): s is OverlaySource => "overlay" in s;
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
/** …and to a found clip's track and to the clip itself, from where `findClip` says it is:
 *  the one spelling of `/bin/<seq>/source/tracks/<i>/clips/<j>`. */
export const trackPathOf = (f: { seq: string; ti: number }): string => `${seqPath(f.seq)}/tracks/${f.ti}`;
export const clipPathOf = (f: { seq: string; ti: number; ci: number }): string => `${trackPathOf(f)}/clips/${f.ci}`;

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

/** Catmull-Rom through four values — MLT's `~=`, and the same formula, so the preview
 *  curves the way the render will. */
const catmull = (p0: number, p1: number, p2: number, p3: number, u: number) => {
  const u2 = u * u;
  return 0.5 * (2 * p1 + (p2 - p0) * u + (2 * p0 - 5 * p1 + 4 * p2 - p3) * u2 + (3 * p1 - p0 - 3 * p2 + p3) * u2 * u);
};

/**
 * Sample a scalar track at `t` (seconds from the clip's start), reading it the way MLT
 * reads an animation string: **the preceding key's ease decides the segment** — held to
 * the next key, a straight line to it, or a curve through the neighbours. Before the first
 * key and after the last, the track holds. `value` reads a key's value, so the keys are
 * sampled as they are rather than copied first: the preview asks this per parameter per
 * frame.
 */
export function sampleTrack<K extends { t: number; ease?: Ease }>(keys: K[], t: number, value: (k: K) => number): number {
  if (!keys.length) return 0;
  if (t <= keys[0].t) return value(keys[0]);
  // At a key's own time the value is the key's: the segment it *opens*, not the one
  // that ends there. This is the whole difference after a hold, and `>` had it wrong.
  let i = 0;
  while (i < keys.length - 1 && t >= keys[i + 1].t - 1e-9) i++;
  const a = keys[i];
  const b = keys[i + 1];
  if (!b || a.ease === "hold") return value(a);
  const u = (t - a.t) / Math.max(1e-9, b.t - a.t);
  if (a.ease !== "smooth") return value(a) + (value(b) - value(a)) * u;
  return catmull(value(keys[i - 1] ?? a), value(a), value(b), value(keys[i + 2] ?? b), u);
}

/** The ease in force at `t`: the key that opens the segment `t` falls in. */
export function easeAt(kfs: { t: number; ease?: Ease }[], t: number): Ease {
  let ease: Ease = "linear";
  for (const k of kfs) {
    if (k.t > t + 1e-9) break;
    ease = k.ease ?? "linear";
  }
  return ease;
}

export type TransformSample = { x: number; y: number; scale: number; rotation: number; opacity: number };

/** An effect's keyframes, for any kind — empty for one that takes none. */
export const keyframesOf = (e: Effect): Keyframe[] => ((e as unknown as { keyframes?: Keyframe[] }).keyframes ?? []);

/** What the effect states for a parameter, or what the parameter means unstated. */
export const staticOf = (e: Effect, param: string): number => {
  const v = (e as unknown as Record<string, unknown>)[param];
  return typeof v === "number" ? v : (ANIMATABLE[e.type]?.[param] ?? 0);
};

/**
 * One parameter of an effect at `local` seconds into the clip: its keyframes read the way
 * the compiler reads them (a key that leaves the parameter out means the static value),
 * or simply the static value when it has none. Every consumer — the live preview, the
 * inspector, a split — asks here, so they cannot disagree about what a clip does at 1.3s.
 */
export function paramAt(e: Effect, param: string, local: number): number {
  const stat = staticOf(e, param);
  const kfs = keyframesOf(e);
  if (!kfs.length) return stat;
  return sampleTrack(kfs, local, (k) => (k[param] as number | undefined) ?? stat);
}

/** Whether `t` falls inside the clip: `end` is exclusive, as everywhere in the model. */
export const covers = (c: { start: number; end: number }, t: number): boolean => t >= c.start && t < c.end;

/** The nearest of `times` that way from `from`, ignoring any within `margin` of it — the
 *  one the playhead may already be standing on. Undefined when there is none. */
export function nearest(times: number[], from: number, dir: -1 | 1, margin: number): number | undefined {
  const ahead = dir < 0 ? times.filter((t) => t < from - margin) : times.filter((t) => t > from + margin);
  if (!ahead.length) return undefined;
  return dir < 0 ? Math.max(...ahead) : Math.min(...ahead);
}

/** Every animatable parameter at `local`, plus the ease in force there: what a key inserted
 *  at that moment carries, so that inserting it moves nothing. */
export function sampleEffect(e: Effect, local: number): Keyframe {
  const key: Keyframe = { t: local };
  for (const param of Object.keys(ANIMATABLE[e.type] ?? {})) key[param] = paramAt(e, param, local);
  const ease = easeAt(keyframesOf(e), local);
  if (ease !== "linear") key.ease = ease;
  return key;
}

/** The transform effect's values at `local` seconds, keyframes applied. */
export function transformAt(e: Extract<Effect, { type: "transform" }>, local: number): TransformSample {
  return { x: paramAt(e, "x", local), y: paramAt(e, "y", local), scale: paramAt(e, "scale", local), rotation: paramAt(e, "rotation", local), opacity: paramAt(e, "opacity", local) };
}

/** Re-base a time-relative effect for a clip that starts `cut` seconds later (the right
 *  half of a split): keyframes shift, the value at the cut becomes the first keyframe. */
export function shiftEffect(e: Effect, cut: number): Effect {
  if (isFade(e)) return { ...e, in: 0 };
  const kfs = keyframesOf(e);
  if (!kfs.length) return e;
  // The key the cut lands in carries that segment's ease (`sampleEffect` does), or a half
  // cut out of a hold would come back interpolating.
  // Only a kind that takes keyframes has any, so the spread lands on one that can hold them.
  return { ...e, keyframes: [{ ...sampleEffect(e, cut), t: 0 }, ...kfs.filter((k) => k.t > cut).map((k) => ({ ...k, t: k.t - cut }))] } as Effect;
}

/** The complementary trim for the left half: keep keyframes up to the cut, then hold. */
export function trimEffect(e: Effect, cut: number): Effect {
  if (isFade(e)) return { ...e, out: 0 };
  const kfs = keyframesOf(e);
  if (!kfs.some((k) => k.t > cut)) return e;
  return { ...e, keyframes: [...kfs.filter((k) => k.t <= cut), sampleEffect(e, cut)] } as Effect;
}

/** Seconds of source a clip's entry has after the clip's out point, when that is knowable —
 *  a probed file or a sequence, as the validator's `source_duration` reads it. A still, a
 *  colour or an overlay is made to any length. What a transition out of it can play. */
export function sourceAfter(p: Project, c: Clip): number | undefined {
  const item = binItem(p, c);
  if (!isTimedMedia(item)) return undefined;
  const total = item && isSequenceSource(item.source) ? seqDuration(p, c.bin) : item?.meta?.duration;
  return total === undefined || total === null ? undefined : total - (c.in ?? 0) - (c.end - c.start) * Math.abs(c.speed || 1);
}
