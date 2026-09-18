// Keyframes, as something you point at rather than type.
//
// The model keeps them as they are: a list of `{ t, …values }` in **clip-local seconds**,
// linear between them (`compile.rs` writes `frame=value;frame=value`, `types.ts`
// interpolates the same way for the live overlay preview). Nothing here changes what a
// keyframe *is*; it is the part that was missing — a way to make one, move one and see
// which one you are editing, instead of a JSON textarea.
//
// Two rules, taken from Kdenlive and Godot respectively, because they are what makes
// keyframing feel like editing rather than data entry:
//
//   - **The playhead is the clock.** A value field shows what that value *is right now*,
//     and writing it writes the key under the playhead — making one there if there is
//     none. There is no "which keyframe am I editing" to keep in your head, because the
//     answer is always "the one you are looking at".
//   - **Animation is opt-in.** A parameter is a plain number until you press ◆, which is
//     what `docs/decisions.md` already agreed for the model's own shape: the panel must
//     not grow a timeline per field.
//
// One thing the model decides for us. A keyframe that leaves a field out does not mean
// "leave that field alone", it means "use the effect's static value" — `k.x.unwrap_or(*x)`
// in the compiler, and the same fallback in `transformAt`. So a key is a **full sample**:
// inserting one writes every animated field at that moment, which is exactly what makes
// inserting one change nothing until you move something. Per-property tracks would need a
// different model (and MLT packs x, y, scale and opacity into one `rect` property anyway).

import { store } from "./store";
import type { Clip, Ease, Effect, Keyframe } from "./types";
import { ANIMATABLE, easeAt, keyframesOf, minKeys, sampleTrack, seqPath, staticOf } from "./types";

/** A keyframe as the model has them — `types.ts` defines it; this is the panel's name. */
export type Key = Keyframe;

/**
 * One field's value at clip-local `t`, read the way the compiler reads it: linear between
 * the keys, and a key that does not carry the field falls back to the effect's own static
 * value rather than to its neighbours.
 */
export function sampleKey(keys: Key[], field: string, t: number, base: number): number {
  if (!keys.length) return base;
  return sampleTrack(keys.map((k) => ({ t: k.t, v: (k[field] as number | undefined) ?? base, ease: k.ease })), t);
}

export interface AnimationOpts {
  /** The effect being edited, as a record, and the field its keys live in. */
  effect: Record<string, unknown>;
  keyField: string;
  /** The numeric fields a key of this effect carries. */
  fields: string[];
  /** How many keys must remain: the volume effect is refused with none. */
  min: number;
  /** The effect's static value for a field — what a key that omits it means. */
  base: (field: string) => number;
  clip: Clip;
  /** Write the effect back, whole, as one patch. */
  commit: (next: Record<string, unknown>, note: string) => void;
}

/**
 * The keyframes of one effect, and everything the panel does to them. Times are read from
 * the store on demand rather than captured, because the playhead moves under a panel that
 * is deliberately not re-rendered while it does.
 */
export class Animation {
  constructor(private o: AnimationOpts) {}

  get keys(): Key[] {
    return (this.o.effect[this.o.keyField] as Key[] | undefined) ?? [];
  }

  /** The numeric fields a key of this effect carries — the ones the panel reads at the
   *  playhead rather than statically. */
  get fields(): string[] {
    return this.o.fields;
  }

  /** How many keys must remain: 1 for a volume effect, which the model refuses empty. */
  get min(): number {
    return this.o.min;
  }

  get animated(): boolean {
    return this.keys.length > 0;
  }

  get duration(): number {
    return this.o.clip.end - this.o.clip.start;
  }

  /** The playhead in clip-local seconds, clamped into the clip. */
  get local(): number {
    return Math.min(this.duration, Math.max(0, store.state.playhead - this.o.clip.start));
  }

  /** Whether it really is in there. Outside, there is no "the value now" to write to, so
   *  the fields are shown read-only and the buttons that would add a key are off. */
  get inside(): boolean {
    const t = store.state.playhead;
    return t >= this.o.clip.start && t < this.o.clip.end;
  }

  /** The key under the playhead, or -1. Within half a frame counts as on it: the playhead
   *  is on the frame grid and a key need not be. */
  get index(): number {
    const half = 0.5 / store.fps;
    return this.keys.findIndex((k) => Math.abs(k.t - this.local) < half);
  }

  /** A field's value at `t` (the playhead by default). */
  at(field: string, t = this.local): number {
    return sampleKey(this.keys, field, t, this.o.base(field));
  }

  /** Every field at `t`, and the ease in force there: what a new key carries, so adding
   *  one moves nothing — including inside a hold, which a linear key would break. */
  sample(t: number): Key {
    const key: Key = { t, ...Object.fromEntries(this.o.fields.map((f) => [f, this.at(f, t)])) };
    const ease = easeAt(this.keys, t);
    if (ease !== "linear") key.ease = ease;
    return key;
  }

  /** How the key under the playhead reaches the next one, and how to change it. */
  get ease(): Ease {
    return this.keys[this.index]?.ease ?? "linear";
  }

  setEase(ease: Ease) {
    const i = this.index;
    if (i < 0) return;
    // `linear` is the default and is not written: a project that does not ease should not
    // grow a field saying so.
    const key = { ...this.keys[i] };
    if (ease === "linear") delete key.ease;
    else key.ease = ease;
    this.write(this.keys.map((k, j) => (j === i ? key : k)), `ease ${ease}`);
  }

  private write(keys: Key[], note: string) {
    // Sorted, because the validator requires it and a retimed key may have passed one.
    this.o.commit({ ...this.o.effect, [this.o.keyField]: [...keys].sort((a, b) => a.t - b.t) }, note);
  }

  /** Write a value at the playhead: into the key there, or into a new one carrying every
   *  other field as it already was at that moment. */
  setAt(field: string, v: number) {
    const i = this.index;
    if (i >= 0) this.write(this.keys.map((k, j) => (j === i ? { ...k, [field]: v } : k)), `${field} at ${this.keys[i].t.toFixed(2)}s`);
    else this.write([...this.keys, { ...this.sample(store.snap(this.local)), [field]: v }], `${field} key`);
  }

  /** Start keyframing: one key here, holding what the effect already does. */
  animate() {
    this.write([this.sample(store.snap(this.local))], "animate");
  }

  addKey(t = this.local) {
    const at = store.snap(Math.min(this.duration, Math.max(0, t)));
    const half = 0.5 / store.fps;
    if (this.keys.some((k) => Math.abs(k.t - at) < half)) return;
    this.write([...this.keys, this.sample(at)], "add key");
  }

  removeKey(i = this.index) {
    if (i < 0 || this.keys.length <= this.o.min) return;
    this.write(this.keys.filter((_, j) => j !== i), "remove key");
  }

  moveKey(i: number, t: number) {
    const at = store.snap(Math.min(this.duration, Math.max(0, t)));
    if (i < 0 || at === this.keys[i]?.t) return;
    this.write(this.keys.map((k, j) => (j === i ? { ...k, t: at } : k)), "move key");
  }

  /** Change one key outright — a value, a time, both — as the curve view does when a
   *  point is dragged in two axes at once. One patch either way. */
  updateKey(i: number, patch: Partial<Key>) {
    if (i < 0 || !this.keys[i]) return;
    const next = { ...this.keys[i], ...patch };
    if (patch.t !== undefined) next.t = store.snap(Math.min(this.duration, Math.max(0, patch.t)));
    this.write(this.keys.map((k, j) => (j === i ? next : k)), "edit key");
  }

  /** Stop keyframing, keeping the picture: the values at the playhead become the static
   *  ones. Without that, un-animating would snap back to whatever the statics were before
   *  the first key — which is never what you meant. */
  clear() {
    if (this.o.min > 0) return;
    // `t` and `ease` belong to a key, not to the effect, which would refuse them.
    const { t: _t, ease: _e, ...values } = this.sample(this.local);
    this.o.commit({ ...this.o.effect, [this.o.keyField]: [], ...values }, "clear keys");
  }

  /** Move the playhead to a clip-local time. */
  seek(t: number) {
    store.seek(this.o.clip.start + t);
  }

  /** To the next key that way, or — from outside the clip — to the nearest end's key. */
  step(dir: -1 | 1) {
    const ts = this.keys.map((k) => k.t);
    if (!ts.length) return;
    if (!this.inside) {
      this.seek(store.state.playhead < this.o.clip.start ? ts[0] : ts[ts.length - 1]);
      return;
    }
    const half = 0.5 / store.fps;
    const ahead = dir < 0 ? ts.filter((t) => t < this.local - half) : ts.filter((t) => t > this.local + half);
    if (ahead.length) this.seek(dir < 0 ? Math.max(...ahead) : Math.min(...ahead));
  }
}

/** The keyframed effects of a clip, as the timeline draws them: which effect, and where
 *  its keys are in clip-local seconds. */
export function keysOf(clip: Clip): { type: string; index: number; times: number[] }[] {
  return clip.effects.map((e, index) => ({ type: e.type, index, times: keyframesOf(e).map((k) => k.t) })).filter((k) => k.times.length > 0);
}

/**
 * The `Animation` for one effect of a clip, wired to write it back through a patch — or
 * null when the clip or the effect is not there (an undo, an agent edit). The inspector
 * and the timeline both build one, so a key dragged on the clip and a key dragged in the
 * panel go through the same code and the same rules.
 */
export function animationOf(clipId: string, ei: number): Animation | null {
  const f = store.findClip(clipId);
  const e = f?.clip.effects[ei];
  if (!f || !e) return null;
  return new Animation({
    effect: e as unknown as Record<string, unknown>,
    keyField: "keyframes",
    fields: Object.keys(ANIMATABLE[e.type] ?? {}),
    min: minKeys(e.type),
    base: (p) => staticOf(e as Effect, p),
    clip: f.clip,
    commit: (next, note) => void store.patch([{ op: "replace", path: `${seqPath(f.seq)}/tracks/${f.ti}/clips/${f.ci}/effects/${ei}`, value: next }], `${e.type} ${note}`),
  });
}
