// Keyframes, as something you point at rather than type.
//
// Nothing here decides what a value at 1.3s *is*: `types.ts` holds the one sampler
// (`paramAt`, `sampleEffect`) that the preview, a split and this panel read through. The
// panel's own two rules are that the **playhead is the clock** — a field shows the value
// there, and writing it writes the key there — and that animation is **opt-in**.
//
// A key that omits a field means the effect's *static* value, not "unchanged", so a key is
// a full sample: inserting one writes every animated field and therefore moves nothing.

import { store } from "./store";
import type { Clip, Ease, Effect, Keyframe } from "./types";
import { ANIMATABLE, clipPathOf, covers, keyframesOf, nearest, paramAt, sampleEffect } from "./types";

/** The keyframes of one effect, and everything the panel does to them. Times are read from
 *  the store on demand, never captured: the playhead moves under a panel that is
 *  deliberately not re-rendered while it does. */
export class Animation {
  /** `commit` writes the effect back, whole, as one patch. */
  constructor(private effect: Effect, private clip: Clip, private commit: (next: Effect, note: string) => void) {}

  get keys(): Keyframe[] {
    return keyframesOf(this.effect);
  }

  /** The numeric fields a key of this effect carries — the ones the panel reads at the
   *  playhead rather than statically. */
  get fields(): string[] {
    return Object.keys(ANIMATABLE[this.effect.type] ?? {});
  }

  get animated(): boolean {
    return this.keys.length > 0;
  }

  get duration(): number {
    return this.clip.end - this.clip.start;
  }

  /** The playhead in clip-local seconds, clamped into the clip. */
  get local(): number {
    return Math.min(this.duration, Math.max(0, store.state.playhead - this.clip.start));
  }

  /** Whether it really is in there. Outside, there is no "the value now" to write to, so
   *  the fields are shown read-only and the buttons that would add a key are off. */
  get inside(): boolean {
    return covers(this.clip, store.state.playhead);
  }

  /** The key under the playhead, or -1. */
  get index(): number {
    return this.keyAt(this.local);
  }

  /** The key at clip-local `t`, or -1. Within half a frame counts as on it: the playhead
   *  is on the frame grid and a key need not be. */
  keyAt(t: number): number {
    return this.keys.findIndex((k) => Math.abs(k.t - t) < store.halfFrame);
  }

  /** A clip-local time as a key may carry it: inside the clip, on the frame grid. */
  clampT(t: number): number {
    return store.snap(Math.min(this.duration, Math.max(0, t)));
  }

  /** A field's value at `t` (the playhead by default). */
  at(field: string, t = this.local): number {
    return paramAt(this.effect, field, t);
  }

  /** Every field at `t`, and the ease in force there: what a new key carries, so adding
   *  one moves nothing — including inside a hold, which a linear key would break. */
  sample(t: number): Keyframe {
    return sampleEffect(this.effect, t);
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

  private write(keys: Keyframe[], note: string) {
    // Sorted, because the validator requires it and a retimed key may have passed one.
    // Only a kind that takes keyframes has an Animation, so the spread lands on one that
    // can hold them.
    this.commit({ ...this.effect, keyframes: [...keys].sort((a, b) => a.t - b.t) } as Effect, note);
  }

  /** Write a value at the playhead: into the key there, or into a new one carrying every
   *  other field as it already was at that moment. */
  setAt(field: string, v: number) {
    const i = this.index;
    if (i >= 0) this.write(this.keys.map((k, j) => (j === i ? { ...k, [field]: v } : k)), `${field} at ${this.keys[i].t.toFixed(2)}s`);
    else this.write([...this.keys, { ...this.sample(this.clampT(this.local)), [field]: v }], `${field} key`);
  }

  /** Start keyframing: one key here, holding what the effect already does. */
  animate() {
    this.write([this.sample(this.clampT(this.local))], "animate");
  }

  addKey(t = this.local) {
    const at = this.clampT(t);
    if (this.keyAt(at) >= 0) return;
    this.write([...this.keys, this.sample(at)], "add key");
  }

  removeKey(i = this.index) {
    if (i < 0) return;
    this.write(this.keys.filter((_, j) => j !== i), "remove key");
  }

  moveKey(i: number, t: number) {
    if (this.keys[i] && this.clampT(t) !== this.keys[i].t) this.updateKey(i, { t }, "move key");
  }

  /** Change one key outright — a value, a time, both — as the curve view does when a
   *  point is dragged in two axes at once. One patch either way. */
  updateKey(i: number, patch: Partial<Keyframe>, note = "edit key") {
    if (i < 0 || !this.keys[i]) return;
    const next = { ...this.keys[i], ...patch };
    if (patch.t !== undefined) next.t = this.clampT(patch.t);
    this.write(this.keys.map((k, j) => (j === i ? next : k)), note);
  }

  /** Stop keyframing, keeping the picture: the values at the playhead become the static
   *  ones. Without that, un-animating would snap back to whatever the statics were before
   *  the first key — which is never what you meant. */
  clear() {
    // `t` and `ease` belong to a key, not to the effect, which would refuse them.
    const { t: _t, ease: _e, ...values } = this.sample(this.local);
    this.commit({ ...this.effect, keyframes: [], ...values } as Effect, "clear keys");
  }

  /** Move the playhead to a clip-local time. */
  seek(t: number) {
    store.seek(this.clip.start + t);
  }

  /** To the next key that way, or — from outside the clip — to the nearest end's key. */
  step(dir: -1 | 1) {
    const ts = this.keys.map((k) => k.t);
    if (!ts.length) return;
    if (!this.inside) {
      this.seek(store.state.playhead < this.clip.start ? ts[0] : ts[ts.length - 1]);
      return;
    }
    const t = nearest(ts, this.local, dir, store.halfFrame);
    if (t !== undefined) this.seek(t);
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
  return new Animation(e, f.clip, (next, note) => void store.patch([{ op: "replace", path: `${clipPathOf(f)}/effects/${ei}`, value: next }], `${e.type} ${note}`));
}
