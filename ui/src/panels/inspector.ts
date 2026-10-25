// Properties panel.
//
// It shows one of two things, and the split is the model's, not a layout choice: what is
// shown lives on the **bin entry** (its name, its file, an overlay's vars) and when it is
// shown lives on the **clip** (start, end, speed, trim, effects). So selecting an entry
// edits the thing itself — and every placement of it follows — while selecting a clip
// edits only that placement. A clip has no name for the same reason; it has a label.

import { api } from "../api";
import { Cmd, run } from "../commands";
import { button, capitalize, checkbox, dragPointer, el, options, plural, row, selectInput } from "../dom";
import { keysFor } from "../shortcuts";
import { desktop } from "../desktop";
import { askForm } from "../modal";
import { panels } from "../panelhost";
import { isEntryTab, store, toast } from "../store";
import type { BinItem, Clip, Cue, Ease, Effect, FontFamily, PatchOp, Project, Streams, Transition, VarSpec } from "../types";
import { blueprintActions, makeOverlayUnique, removeTransition, setTransitionDuration, setTransitionKind, updateTransition, manifestOf, moveBinItem, overlayError, placeBinItem, previewImg, renderPreview, saveAsBlueprint, sharersOf } from "../edits";
import { Animation, animationOf } from "../keyframes";
import { historyFor } from "./history";
import { icon, svgEl } from "../icons";
import type { NumberOpts } from "../props";
import { keyStrip, numberField, showNumber, sliderField, vectorField } from "../props";
import { LUMAS, SEQUENCE_EFFECTS, sourceAfter, actsOn, ANIMATABLE, binItem, binPath, canPlace, canTimeWarp, childrenOf, clipStreams, covers, expandPreset, hasAudio, isColorSource, isFileSource, isFolderSource, isOverlaySource, isSequenceSource, isTimedMedia, itemKind, mediaFacts, placementsOf, seqDuration, seqPath, sequenceOf, sequenceStreams, streamsOf, wouldNest } from "../types";

const presetsPromise = api.effectPresets().catch(() => []);

/** What one field of an effect looks like. The shapes come from `props.ts` — a number you
 *  drag, a vector wearing its axis colours, a bounded amount with a slider — and this says
 *  which shape each field is. Nothing else in the panel needs to know. */
type PropSpec =
  | ({ kind: "num"; key: string; label?: string } & NumberOpts)
  | ({ kind: "slider"; key: string; label?: string; min: number; max: number; /** The range is a suggestion, not a limit: the field may be typed past it. */ soft?: boolean } & NumberOpts)
  | { kind: "vec"; label: string; parts: ({ key: string } & NumberOpts)[] }
  | { kind: "enum"; key: string; label?: string; options: string[] }
  | { kind: "text"; key: string; label?: string; /** Empty means the field is left out, not an empty string. */ optional?: boolean }
  | { kind: "json"; key: string; label?: string };

/** Per effect kind: the value `+ add effect` inserts, and the properties it shows, in
 *  order. Its name is its type, spaced (`title`). Adding a kind is one row here plus the
 *  Rust model (and its streams in `types.ts`); one the daemon knows and this table does not
 *  renders with no fields. A kind in `ANIMATABLE` gets the keyframe toggle in its head, and
 *  the keyframe row once it has keys. Units are the model's, and the tooltips say so where
 *  it is not obvious. */
interface EffectSpec { add: Effect; props?: PropSpec[] }
const EASES: Ease[] = ["linear", "hold", "smooth"];
/** A fade's two lengths, whichever stream it fades. */
const fadeProps = (into: string, outOf: string): PropSpec[] => [{ kind: "vec", label: "Duration", parts: [
  { key: "in", tag: "In", step: 0.1, min: 0, suffix: "s", title: `Seconds to ${into}` },
  { key: "out", tag: "Out", step: 0.1, min: 0, suffix: "s", title: `Seconds to ${outOf}` },
] }];
/** A grade's per-channel wheel: a row of R, G and B, neutral at 0, addressed as `lift.r`. */
const wheel = (key: string, what: string): PropSpec => ({ kind: "vec", label: capitalize(key), parts: (["r", "g", "b"] as const).map((ch, i) => (
  { key: `${key}.${ch}`, tag: ch.toUpperCase(), axis: (["x", "y", "z"] as const)[i], step: 0.01, min: -1, max: 1, title: `${what}, on ${["red", "green", "blue"][i]}` }
)) });
const BLEND_MODES = ["normal", "multiply", "screen", "overlay", "darken", "lighten", "color_dodge", "color_burn", "hard_light", "soft_light", "difference", "exclusion"];
const EFFECTS: Partial<Record<Effect["type"], EffectSpec>> = {
  transform: {
    add: { type: "transform", ...ANIMATABLE.transform, keyframes: [] },
    props: [
      { kind: "vec", label: "Position", parts: [
        { key: "x", tag: "X", axis: "x", step: 1, suffix: "px", title: "Pixels right of the centred position" },
        { key: "y", tag: "Y", axis: "y", step: 1, suffix: "px", title: "Pixels down from the centred position" },
      ] },
      // The model's own bound: a scale of 0 is refused, so the field stops at one step.
      { kind: "num", key: "scale", step: 0.01, min: 0.01, suffix: "×", title: "Multiplies the composition size" },
      { kind: "num", key: "rotation", step: 1, suffix: "°" },
      { kind: "slider", key: "opacity", min: 0, max: 1, step: 0.01 },
    ],
  },
  crop: {
    add: { type: "crop", ...ANIMATABLE.crop },
    props: [
      { kind: "vec", label: "Edges", parts: [
        { key: "left", tag: "L", axis: "x", step: 0.01, min: 0, max: 1, title: "Fraction cut off the left edge" },
        { key: "right", tag: "R", axis: "y", step: 0.01, min: 0, max: 1, title: "Fraction cut off the right edge" },
        { key: "top", tag: "T", axis: "z", step: 0.01, min: 0, max: 1, title: "Fraction cut off the top edge" },
        { key: "bottom", tag: "B", axis: "w", step: 0.01, min: 0, max: 1, title: "Fraction cut off the bottom edge" },
      ] },
      { kind: "slider", key: "radius", min: 0, max: 1, step: 0.01, title: "Corner radius, as a fraction of half the shorter side of what the crop leaves: 1 rounds a square into a circle. The crop is of the whole frame, so crop a letterboxed still to its edges first" },
    ],
  },
  blend: { add: { type: "blend", mode: "screen" }, props: [{ kind: "enum", key: "mode", options: BLEND_MODES }] },
  blur: { add: { type: "blur", radius: 4 }, props: [{ kind: "num", key: "radius", step: 0.5, min: 0, suffix: "px" }] },
  color: {
    add: { type: "color", ...ANIMATABLE.color },
    // Neutral is 0, 1, 1, 1. The ranges are what the eye uses; `soft` lets a typed value
    // go past them, because the filter underneath takes more than the slider offers.
    props: [
      { kind: "slider", key: "brightness", min: -1, max: 1, step: 0.01, soft: true },
      { kind: "slider", key: "contrast", min: 0, max: 3, step: 0.01, soft: true },
      { kind: "slider", key: "saturation", min: 0, max: 3, step: 0.01, soft: true },
      { kind: "slider", key: "gamma", min: 0.1, max: 4, step: 0.01, soft: true },
    ],
  },
  // Compiled as one LUT, in this order (`kuma_core::grade`), so the rows read top to bottom
  // the way a colour goes through it. Nothing here animates: a grade is the clip's, and
  // grading two halves differently is a split.
  grade: {
    add: { type: "grade", temperature: 0, tint: 0, exposure: 0, contrast: 1, curve: [], saturation: 1, look_amount: 1 },
    props: [
      { kind: "text", key: "input_lut", label: "Input LUT", optional: true },
      { kind: "slider", key: "temperature", min: -1, max: 1, step: 0.01, title: "Warmer above 0, cooler below: red and blue a stop apart at ±1" },
      { kind: "slider", key: "tint", min: -1, max: 1, step: 0.01, title: "Towards magenta above 0, green below" },
      { kind: "slider", key: "exposure", min: -5, max: 5, step: 0.05, suffix: "EV", title: "Stops of light: 1 doubles it" },
      wheel("lift", "Raises the shadows, white stays"),
      wheel("gamma", "Bends the midtones, black and white stay"),
      wheel("gain", "Scales the highlights, black stays"),
      { kind: "slider", key: "contrast", min: 0, max: 3, step: 0.01, title: "About mid grey: 1 leaves it" },
      { kind: "json", key: "curve", label: "Curve" },
      { kind: "slider", key: "saturation", min: 0, max: 3, step: 0.01, title: "1 leaves it, 0 is black and white" },
      { kind: "text", key: "look", label: "Look LUT", optional: true },
      { kind: "slider", key: "look_amount", label: "Look amount", min: 0, max: 1, step: 0.01 },
    ],
  },
  // The picture's fade and the sound's are two kinds, so each clip is offered the one it
  // can use, and a clip with both can give them different lengths.
  fade: { add: { type: "fade", in: 0.5, out: 0.5 }, props: fadeProps("fade in from transparent", "fade out to transparent") },
  audio_fade: { add: { type: "audio_fade", in: 0.5, out: 0.5 }, props: fadeProps("rise from silence", "fall to silence") },
  // A gain on top of the clip's own Volume, which is the placement's fader: this is the
  // one that moves, once it is animated.
  volume: {
    add: { type: "volume", ...ANIMATABLE.volume },
    props: [
      { kind: "slider", key: "gain", min: 0, max: 2, step: 0.01, soft: true, title: "1 is the clip as recorded; it multiplies the clip's own Volume" },
    ],
  },
  // Holds sample peaks at the ceiling and raises nothing: the gain in front of it does that.
  limiter: {
    add: { type: "limiter", ceiling: -1, attack: 5, release: 50 },
    props: [
      { kind: "num", key: "ceiling", step: 0.1, min: -24, max: 0, suffix: "dB", title: "The highest a sample may reach, in dBFS. True peaks land up to about a dB over it" },
      { kind: "num", key: "attack", step: 0.5, min: 0.1, max: 80, suffix: "ms", title: "How far ahead it looks — and so how much later the sound comes out" },
      { kind: "num", key: "release", step: 5, min: 1, max: 8000, suffix: "ms", title: "How long the gain takes to come back up" },
    ],
  },
  auto_level: {
    add: { type: "auto_level", target: -23, window: 3, max_gain: 15, min_gain: -15 },
    props: [
      { kind: "num", key: "target", step: 0.5, min: -50, max: -10, suffix: "LUFS", title: "The loudness it rides the gain toward" },
      { kind: "num", key: "window", step: 0.5, min: 1, max: 60, suffix: "s", title: "How much of the past it measures: longer is steadier and slower to follow" },
      { kind: "num", key: "max_gain", label: "Most up", step: 1, min: 0, max: 30, suffix: "dB" },
      { kind: "num", key: "min_gain", label: "Most down", step: 1, min: -30, max: 0, suffix: "dB" },
    ],
  },
  freeze: {
    add: { type: "freeze", t: 0 },
    props: [{ kind: "num", key: "t", label: "Hold at", step: 1 / 30, suffix: "s", title: "Seconds from the clip's start: the frame shown there is held for the whole clip, while the sound runs on" }],
  },
  mlt: { add: { type: "mlt", service: "frei0r.vignette", properties: {} }, props: [{ kind: "text", key: "service" }, { kind: "json", key: "properties" }] },
};

/** What carries a list of effects — a clip, or a sequence on its mix — and how the panel
 *  reaches it. One editor for both, so an effect looks and keys the same wherever it is. */
interface EffectOwner {
  /** Names it in view state (folds, curves): a clip id, or `seq:<id>`. */
  key: string;
  /** How a note says it: "the clip". */
  what: string;
  effects: Effect[];
  /** Where its effects list is in the document. */
  path: string;
  /** What it can show and sound: an effect on neither is dimmed. */
  carries: Streams;
  /** The kinds `+ add effect` offers it. */
  offers: (type: Effect["type"]) => boolean;
  anim: (ei: number) => Animation | null;
  /** What a new effect of a kind starts as, here. */
  add: (type: Effect["type"]) => Effect;
}

/** Which effects have their curve view open, and on which parameter — by `clip/effect`,
 *  so the view survives the re-render every edit causes. View state; nothing saves it. */
const openCurves = new Map<string, string>();

/** Which effects are folded shut, by the same `clip/effect`. *Closed* is what is stored,
 *  so a new effect and a newly selected clip open. View state, like the curves. */
const closedEffects = new Set<string>();

/** How many points the curve is drawn with. Enough for a Catmull-Rom to look like one. */
const CURVE_POINTS = 64;

/** One parameter's value over the clip, its keys as points you can drag in both axes. The
 *  graph is what the strip cannot say: the *shape* between keys, which is the whole
 *  question with a hold or a smooth. It draws the curve the compiler will produce
 *  (`paramAt` is the preview's own sampler); it does not let you draw one. */
function curveView(anim: Animation, param: string, onParam: (p: string) => void, follow: (fn: () => void) => void): HTMLElement {
  const wrap = el("div", "kf-curve");
  if (anim.fields.length > 1) {
    const pick = selectInput(anim.fields.map((value) => ({ value, label: title(value) })), param, onParam);
    pick.className = "kf-curve-param";
    wrap.appendChild(pick);
  }
  const plot = el("div", "kf-plot");
  const dur = anim.duration;

  // On a grid, at every key, and a hair *before* every key — so a hold draws as the step
  // it is rather than a slant one grid-cell wide.
  const times = new Set<number>(Array.from({ length: CURVE_POINTS + 1 }, (_, i) => (i / CURVE_POINTS) * dur));
  for (const k of anim.keys) { times.add(Math.max(0, k.t - 1e-6)); times.add(k.t); }
  const grid = [...times].sort((a, b) => a - b).map((t) => ({ t, v: anim.at(param, t) }));
  const samples = grid.map((g) => g.v);
  // The axis fits the curve, which passes through every key, with a little room; a flat
  // line gets a unit either side rather than a division by nothing.
  let lo = Math.min(...samples);
  let hi = Math.max(...samples);
  if (hi - lo < 1e-9) { lo -= 1; hi += 1; }
  const pad = (hi - lo) * 0.12;
  lo -= pad;
  hi += pad;
  const xPct = (t: number) => (t / Math.max(dur, 1e-6)) * 100;
  const yPct = (v: number) => (1 - (v - lo) / (hi - lo)) * 100;

  // The line is SVG stretched to the box; the points are HTML, so they keep their shape.
  const svg = svgEl("svg", { viewBox: "0 0 100 100", preserveAspectRatio: "none" });
  svg.appendChild(svgEl("polyline", { points: grid.map((g) => `${xPct(g.t)},${yPct(g.v)}`).join(" ") }));
  plot.appendChild(svg);

  const head = el("div", "kf-plot-head");
  const showHead = () => { head.hidden = !anim.inside; if (anim.inside) head.style.left = `${xPct(anim.local)}%`; };
  showHead();
  follow(showHead);
  plot.appendChild(head);

  anim.keys.forEach((k, i) => {
    const v = anim.at(param, k.t);
    const point = el("div", `kf-point${k.ease && k.ease !== "linear" ? ` ${k.ease}` : ""}`);
    point.style.left = `${xPct(k.t)}%`;
    point.style.top = `${yPct(v)}%`;
    point.title = `${k.t.toFixed(2)}s · ${showNumber(v)} — drag: sideways for when, up and down for how much`;
    point.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      const r = plot.getBoundingClientRect();
      let at = k.t;
      let val = v;
      dragPointer(e, {
        axes: "xy",
        move: (dx, dy) => {
          at = Math.min(dur, Math.max(0, k.t + (dx / r.width) * dur));
          val = Number((v - (dy / r.height) * (hi - lo)).toFixed(3));
          point.style.left = `${xPct(at)}%`;
          point.style.top = `${yPct(val)}%`;
        },
        end: (moved) => {
          if (moved) anim.updateKey(i, { t: at, [param]: val });
          else anim.seek(k.t);
        },
      });
    });
    plot.appendChild(point);
  });

  // The value axis, as the two numbers that matter: the top of the box and the bottom.
  const top = el("span", "kf-axis hi muted tiny", showNumber(hi - pad));
  const bottom = el("span", "kf-axis lo muted tiny", showNumber(lo + pad));
  plot.append(top, bottom);
  wrap.appendChild(plot);
  return wrap;
}

/** Whether the playhead is inside the selected clip. */
function playheadInClip(): boolean {
  const f = store.findClip(store.selectedClip);
  return !!f && covers(f.clip, store.state.playhead);
}

/** A key as a label: `brightness` → `Brightness`, `audio_fade` → `Audio fade`. */
const title = (key: string) => capitalize(key.replace(/_/g, " "));

/** How a row reads and writes its effect: the static fields, or — once there are keys —
 *  the value *at the playhead*, written into the key there. */
interface PropCtx {
  read: (key: string) => unknown;
  write: (key: string, v: unknown) => void;
  /** What a numeric field needs on top of its own spec when the value is keyed: it is
   *  read-only while the playhead is outside the clip (there is no "now" to write to),
   *  and it follows the playhead. Empty for a field that is not keyed. */
  timed: (key: string) => Pick<NumberOpts, "readOnly" | "live">;
}

/** One property row, whatever shape it is. What it edits is on the row: the panel styles
 *  by it, and a test can name a property rather than counting divs. */
function propRow(p: PropSpec, ctx: PropCtx): HTMLElement {
  const { read, write, timed } = ctx;
  const num = (key: string) => Number(read(key) ?? 0);
  const label = p.kind === "vec" ? p.label : p.label ?? title(p.key);
  let input: HTMLElement;
  switch (p.kind) {
    case "vec":
      input = vectorField(p.parts.map((part) => ({ ...part, ...timed(part.key), value: num(part.key), commit: (v: number) => write(part.key, v) })));
      break;
    case "num":
      input = numberField(num(p.key), (v) => write(p.key, v), { ...p, ...timed(p.key) });
      break;
    case "slider":
      input = sliderField(num(p.key), (v) => write(p.key, v), { ...p, ...timed(p.key) });
      break;
    case "enum":
      input = selectInput(p.options.map((value) => ({ value })), String(read(p.key)), (v) => write(p.key, v));
      break;
    case "text":
      input = textInput(String(read(p.key) ?? ""), (v) => write(p.key, v === "" && p.optional ? undefined : v));
      break;
    case "json":
      input = jsonInput(read(p.key) ?? null, (v) => write(p.key, v));
      break;
  }
  const row = field(label, input);
  row.dataset.prop = p.kind === "vec" ? p.label.toLowerCase() : p.key;
  return row;
}

/** The keyframe toggle, in the effect's head: an outlined diamond on an effect that could
 *  be keyframed, a filled one on an effect that is. It is how a keyframed effect says so
 *  while folded, and the one control for starting and stopping — a row of its own under
 *  every effect was a large button for something most effects never want. */
function keyToggle(anim: Animation): HTMLButtonElement {
  const on = anim.animated;
  const b = button(icon(on ? "key" : "key-open", 11), () => (on ? anim.clear() : anim.animate()),
    on ? "Stop animating" : "Animate", "kf-toggle");
  b.classList.toggle("on", on);
  b.setAttribute("aria-pressed", String(on));
  return b;
}

/** The keyframe row: what there is to do to an effect's keys, and where they are — for an
 *  effect that has some. One row of buttons and a strip; the panel deliberately does not
 *  grow a track editor, and the clip in the timeline carries the same keys. */
function keyRow(anim: Animation, id: string, follow: (fn: () => void) => void): HTMLElement {
  const wrap = el("div");
  wrap.dataset.prop = "keys";

  // The whole width, not the value column: it is about the effect's timing rather than one
  // of its values, and four buttons do not fit there.
  const tools = el("div", "kf-tools");
  const prev = button(icon("chevron-left", 12), () => anim.step(-1), "Previous key", "icon");
  const next = button(icon("chevron-right", 12), () => anim.step(1), "Next key", "icon");
  const add = button("+ key", () => anim.addKey(), "Add a key at the playhead, holding the values it has there");
  const del = button("− key", () => anim.removeKey(), "Remove the key at the playhead");
  // Off until asked for, and it stays put across the re-render every edit causes.
  const curveHost = el("div", "kf-curve-host");
  const curve = button("curve", () => {
    if (openCurves.has(id)) openCurves.delete(id);
    else openCurves.set(id, anim.fields[0]);
    showCurve();
  }, "Show this effect's values over the clip as a graph, with the keys to drag");
  // One follower for whichever curve is showing: the view is rebuilt on every toggle and
  // parameter pick, and each build must not leave the last one's marker following a
  // detached graph.
  let curveFollower: (() => void) | null = null;
  follow(() => curveFollower?.());
  const showCurve = () => {
    const param = openCurves.get(id);
    curve.classList.toggle("on", !!param);
    curveFollower = null;
    curveHost.replaceChildren(...(param ? [curveView(anim, param, (p) => { openCurves.set(id, p); showCurve(); }, (fn) => { curveFollower = fn; })] : []));
  };
  // No caption: the strip under it says what these are, and the width is spoken for.
  // Stopping is the diamond in the head, which is also where starting was.
  tools.append(prev, add, del, next, curve);
  wrap.appendChild(tools);

  wrap.appendChild(keyStrip({
    duration: anim.duration,
    // The shape says how the key reaches the next one: a diamond runs straight there, a
    // square holds, a circle curves — Kdenlive's shapes, for the same reason.
    keys: anim.keys.map((k) => ({ t: k.t, kind: k.ease })),
    playhead: anim.inside ? anim.local : null,
    onSeek: (t) => anim.seek(t),
    onMove: (i, t) => anim.moveKey(i, t),
    onAdd: (t) => anim.addKey(t),
    snap: (t) => store.snap(t),
    live: (set) => follow(() => set(anim.inside ? anim.local : null)),
  }));

  // A property of the key under the playhead, so it sits under the strip rather than among
  // the effect's values, and means nothing when the playhead is not on one.
  const ease = selectInput(EASES.map((value) => ({ value })), anim.ease, (v) => anim.setEase(v as Ease));
  ease.className = "kf-ease";
  ease.title = "How the key under the playhead reaches the next one: a straight line, a hold, or a curve through its neighbours. It takes effect as you pick it.";
  // "Ease → next", not "Ease": it describes the *segment* this key opens. "Ease" alone
  // reads as the key's own, and then setting it on the last key looks like a bug.
  wrap.appendChild(field("Ease → next", ease));

  wrap.appendChild(curveHost);
  showCurve();

  const hint = el("div", "muted tiny kf-hint");
  const refresh = () => {
    const i = anim.index;
    // The last key opens no segment, so its ease has nothing to describe. Rather than let
    // it be set and do nothing — which is what it looked like — the control is off there
    // and says where the ease you want lives.
    const last = i >= 0 && i === anim.keys.length - 1;
    add.disabled = !anim.inside || i >= 0;
    del.disabled = !anim.inside || i < 0;
    ease.disabled = i < 0 || last;
    ease.value = anim.keys[i]?.ease ?? "linear";
    hint.textContent = !anim.inside
      ? "The playhead is outside this clip — click the strip, or use ‹ ›, to edit a key."
      : i < 0
        ? "Ease belongs to a key: go to one with ‹ › to set how it reaches the next."
        : last
          ? "The last key reaches nothing, so it has no ease. Set it on the key before (‹) to shape the run into this one."
          : "";
  };
  refresh();
  follow(refresh);
  wrap.appendChild(hint);
  return wrap;
}

export class Inspector {
  /** Subscriptions this render made — keyed values following the playhead — dropped when
   *  the panel is rebuilt, or they would pile up one per render. */
  private off: (() => void)[] = [];

  constructor(private host: HTMLElement) {
    // The playhead is in here as a *flag*, not as a value: whether it is inside the
    // selected clip decides what the panel can edit at all — a keyed value has no "now" to
    // write to from outside it — and that flips rarely. The values themselves follow the
    // playhead without a re-render; see `renderEffects`.
    // Rebuilt while it is on screen; in den, unsummoned, it waits to be.
    store.select((s) => [s.project, s.selection, s.manifests, s.fonts, s.blueprints, s.sequence, playheadInClip(), s.previewStamp], panels.lazily("inspector", () => this.render()).run);
  }

  private render() {
    for (const stop of this.off) stop();
    this.off = [];
    const sel = store.state.selection;
    this.host.innerHTML = "";
    if (sel?.kind === "bin") return this.renderBinEntry(sel.id);
    if (sel?.kind === "blueprint") return this.renderBlueprint(sel.id);
    if (sel?.kind === "transition") return this.renderTransition(sel);
    const f = store.findClip(store.selectedClip);
    if (!f) {
      this.host.innerHTML = `<h3>Inspector</h3>
        <p class="muted">Select a <b>bin entry</b> to change what it is, or a <b>clip</b> to change where and how it plays. Drag from the bin onto the timeline to place one.</p>`;
      // Not a list of keys copied out here: there is one, it is the bindings themselves,
      // and two copies of it would disagree within a week.
      this.host.appendChild(button("Keyboard shortcuts", () => run(Cmd.shortcuts), `Every key and gesture the editor has (${keysFor(Cmd.shortcuts)})`));
      this.renderSequenceEffects();
      return;
    }
    const { clip } = f;
    const path = store.clipPath(clip.id)!;
    const rep = (sub: string, value: unknown, note: string) => store.patch([{ op: "replace", path: `${path}${sub}`, value }], note);

    const item = binItem(store.state.project!, clip);
    const h = el("h3", "insp-kicker", "Clip ");
    h.appendChild(el("span", "mono muted", clip.id));
    // What it is and where: its label (or what it plays), then the kind and the track.
    const head = el("div", "insp-head");
    head.append(h, el("div", "insp-name", clip.label || item?.name || clip.bin), el("div", "insp-sub", `${f.track.kind} · ${f.track.name || f.track.id}`));
    this.host.appendChild(head);
    // With several selected the panel still edits one — the last one clicked. Saying so is
    // the honest version: the alternative is fields that look like they apply to all of
    // them. Copy, cut, delete and split do apply to all of them, and say so in the strip.
    const n = store.selectedClips.length;
    this.host.appendChild(el("p", "muted tiny", n > 1 ? `This placement only — ${n} clips are selected, and this is the last one clicked.` : "This placement only."));

    const frame = 1 / store.fps;
    this.host.appendChild(field("Start", numberField(clip.start, (v) => void rep("/start", v, "start"), { step: frame, min: 0, suffix: "s" })));
    this.host.appendChild(field("End", numberField(clip.end, (v) => void rep("/end", v, "end"), { step: frame, min: 0, suffix: "s" })));
    // MLT cannot time-warp a timeline, so the field would only ever be refused.
    if (canTimeWarp(item)) {
      // ±[0.01, 20] in the model; the ends are what a field can say, and the daemon still
      // has the last word on the gap in the middle.
      this.host.appendChild(field("Speed", numberField(clip.speed, (v) => void rep("/speed", v, "speed"), { step: 0.05, min: -20, max: 20, suffix: "×" })));
    }
    if (isTimedMedia(item)) {
      this.host.appendChild(field("Source in", numberField(clip.in ?? 0, (v) => void rep("/in", v, "in"), { step: frame, min: 0, suffix: "s" })));
    }
    // What the clip can show and sound: which effects it is offered, and whether it has a
    // level to set at all. A capability, so a muted clip keeps its Muted box.
    const carries = clipStreams(store.state.project!, f.track, clip);
    if (carries.audio) {
      this.host.appendChild(field("Volume", sliderField(clip.volume, (v) => void rep("/volume", v, "volume"), { min: 0, max: 2, step: 0.01, soft: true, title: "Gain for this placement, on top of its bin entry's: 1 is the clip as recorded" })));
      this.host.appendChild(field("Muted", checkbox(clip.muted, (v) => void rep("/muted", v, "mute"))));
    }
    this.host.appendChild(field("Label", textInput(clip.label, (v) => rep("/label", v, "label"))));

    // Not the entry's fields: those belong to every other placement too. A way through
    // to them instead, so "where do I change the text" has an answer from here.
    if (item) {
      const go = el("div", "field");
      go.append(el("label", "", "Plays"), linkButton(`${item.name || clip.bin} →`, () => store.selectItem("bin", clip.bin), "Edit the bin entry (changes every clip that plays it)"));
      this.host.appendChild(go);
    }
    this.renderEffects({
      key: clip.id,
      what: "the clip",
      effects: clip.effects,
      path: `${path}/effects`,
      carries,
      // A freeze holds a frame of something that moves: a colour or a still is held already.
      offers: (type) => actsOn(type, carries) && (type !== "freeze" || isTimedMedia(item) || (!!item && isOverlaySource(item.source))),
      anim: (ei) => animationOf(clip.id, ei),
      // A freeze starts on the frame under the playhead, when the playhead is on the clip.
      add: (type) => (type === "freeze" && covers(clip, store.state.playhead) ? { type, t: store.snap(store.state.playhead - clip.start) } : EFFECTS[type]!.add),
    });
    this.host.appendChild(this.presetPicker(path));

    const actions = el("div", "insp-actions");
    // The command reads the selection: from a clip the subtitles land over it, aligned.
    if (hasAudio(item)) actions.appendChild(button("Transcribe → subtitles", () => run(Cmd.transcribe), "Transcribe the audio and place aligned subtitles over this clip"));
    // Grouping is Ctrl+G, and a keyboard shortcut is not a way to find out a feature
    // exists — the desktop shell has a menu for it and a browser tab has nothing.
    if (clip.group) {
      actions.appendChild(button("Ungroup", () => run(Cmd.ungroup), `These ${plural(n, "clip")} move together (${clip.group}). Ungroup them (${keysFor(Cmd.ungroup)})`));
    } else if (n > 1) {
      actions.appendChild(button("Group", () => run(Cmd.group), `Hold these clips together: selecting one selects them all, and a drag moves the block (${keysFor(Cmd.group)})`));
    }
    actions.appendChild(button("Delete clip", () => store.patch([{ op: "remove", path }], `delete ${clip.id}`)));
    this.host.appendChild(actions);
    const past = historyFor([clip.id], "this clip");
    this.off.push(past.off);
    this.host.appendChild(past.box);
  }

  /**
   * A transition: its kind, length and a wipe's pattern, and the handles it plays — the
   * footage beyond the two clips' edit points, which is what the clips being end to end
   * in the document costs, and so what is worth being told about.
   */
  private renderTransition(sel: { track: string; from: string; to: string }) {
    const p = store.state.project!;
    const found = store.findTransition(p, sel);
    const a = found?.track.clips.find((c) => c.id === sel.from);
    const b = found?.track.clips.find((c) => c.id === sel.to);
    if (!found || !a || !b) return;
    const { ti, x } = found;
    const frame = 1 / store.fps;
    const name = (c: Clip) => c.label || binItem(p, c)?.name || c.bin;

    this.host.appendChild(el("h3", "", x.kind === "wipe" ? "Wipe" : "Dissolve"));
    this.host.appendChild(el("p", "muted tiny", `From “${name(a)}” into “${name(b)}”, centred on the cut at ${b.start.toFixed(2)}s. The clips stay end to end; the transition plays the footage beyond their edit points.`));
    this.host.appendChild(field("Kind", selectInput([{ value: "dissolve" }, { value: "wipe" }], x.kind, (v) => void setTransitionKind(ti, x, v as Transition["kind"]))));
    this.host.appendChild(field("Length", numberField(x.duration, (v) => void setTransitionDuration(ti, x, v), { step: frame, min: frame, suffix: "s" })));
    if (x.kind === "wipe") {
      // A pattern file under the project is kept and shown, though not offered.
      const lumas = [...LUMAS, ...(x.luma && !LUMAS.includes(x.luma) ? [x.luma] : [])];
      this.host.appendChild(field("Pattern", selectInput(lumas.map((value) => ({ value, label: value.replace(/_/g, " ") })), x.luma ?? LUMAS[0], (v) => void updateTransition(ti, x, { luma: v }, `pattern ${v}`))));
      this.host.appendChild(field("Softness", sliderField(x.softness ?? 0, (v) => void updateTransition(ti, x, { softness: v }, `softness ${v}`), { min: 0, max: 1, step: 0.01, title: "How soft the wipe's edge is: 0 is a hard line" })));
    }

    // Each side's handle: what it needs (half the length, in source time) and what it has.
    this.host.appendChild(el("h3", "", "Handles"));
    const half = x.duration / 2;
    const handle = (label: string, c: Clip, has: number | undefined, fix: string) => {
      const need = half * Math.abs(c.speed || 1);
      const short = has !== undefined && has < need - 1e-6;
      const text = has === undefined ? "any length — made to measure" : `needs ${need.toFixed(2)}s, has ${Math.max(0, has).toFixed(2)}s${short ? ` — ${fix}` : ""}`;
      const line = el("div", "field");
      const say = el("span", short ? "warn" : "muted", text);
      line.append(linkButton(`${label} “${name(c)}”`, () => store.selectClips([c.id], sel.track), "Select this clip"), say);
      this.host.appendChild(line);
    };
    handle("Before", b, isTimedMedia(binItem(p, b)) ? b.in ?? 0 : undefined, "trim its start, or shorten the transition");
    handle("After", a, sourceAfter(p, a), "its end will be black: trim it, or shorten the transition");

    const actions = el("div", "insp-actions");
    actions.append(
      button("Go to the cut", () => store.seek(b.start)),
      button("Remove transition", () => void removeTransition(ti, x)),
    );
    this.host.appendChild(actions);
  }

  /** A blueprint in the library: what it is, and the ways into the project. */
  private renderBlueprint(id: string) {
    const bp = store.state.blueprints.find((b) => b.id === id);
    if (!bp) return;
    const m = bp.manifest;
    const h = el("h3", "", "Blueprint ");
    h.appendChild(el("span", "mono muted", id));
    this.host.appendChild(h);
    this.host.appendChild(field("Name", el("div", "", m.name || id)));
    if (m.description) this.host.appendChild(el("p", "muted", m.description));
    this.host.appendChild(previewImg(`/blueprints/${encodeURIComponent(id)}`));
    this.host.appendChild(field("Vars", el("div", "mono muted", Object.keys(m.vars).join(" · ") || "none")));
    this.host.appendChild(field("Length", el("div", "mono muted", `${m.duration_default}s${m.static ? " · still" : ""}`)));
    if (m.tags.length) this.host.appendChild(field("Tags", el("div", "mono muted", m.tags.join(" · "))));
    const actions = blueprintActions(id, "insp-actions");
    actions.appendChild(button("Preview", () => void renderPreview({ blueprints: [id] }), "Render its animated preview"));
    this.host.appendChild(actions);
  }

  /**
   * A bin entry: what it *is*. Every clip that places it follows these, which is why the
   * panel says so rather than leaving it to be discovered by editing one and watching
   * another change.
   */
  private renderBinEntry(id: string) {
    const p = store.state.project!;
    const item = p.bin[id];
    if (!item) return;
    const bp = binPath(id);
    const h = el("h3", "", "Bin entry ");
    h.appendChild(el("span", "mono muted", id));
    this.host.appendChild(h);

    const places = placementsOf(p, id);
    this.host.appendChild(el("p", "muted tiny", places === 0 ? "Not placed on any timeline yet." : `Placed by ${plural(places, "clip")} — these apply to all of them.`));
    // Where it is listed: any folder, or wherever it already is (a transcript under its
    // media), never inside itself.
    const choices = Object.entries(p.bin).filter(([pid, i]) => (isFolderSource(i.source) || pid === item.parent) && !wouldNest(p, id, pid));
    this.host.appendChild(field("Under", selectInput(choices.map(([pid, i]) => ({ value: pid, label: i.name || pid })), item.parent, (v) => void moveBinItem(id, v || null, null), "(top level)")));

    this.host.appendChild(field("Name", textInput(item.name, (v) => store.patch([{ op: "replace", path: `${bp}/name`, value: v }], `name ${id}`))));
    this.renderSource(p, id, item, bp);
    if (isTimedMedia(item)) {
      this.host.appendChild(field("Volume", sliderField(item.volume ?? 1, (v) => void store.patch([{ op: "add", path: `${bp}/volume`, value: v }], `volume ${id}`), { min: 0, max: 2, step: 0.01, soft: true, title: "Gain for every placement of this entry: 1 is the source as recorded" })));
    }

    const actions = el("div", "insp-actions");
    if (canPlace(p, id, store.state.sequence)) {
      actions.appendChild(button("Add at playhead", () => placeBinItem(id, store.state.playhead)));
    }
    // From the entry the subtitles are listed under it and left for you to place; to get
    // them aligned on the timeline, transcribe from a clip instead.
    if (hasAudio(item)) actions.appendChild(button("Transcribe → subtitles", () => run(Cmd.transcribe), "Run the local speech-to-text model and add a subtitles entry under this one"));
    if (isSequenceSource(item.source)) {
      const open = button("Open", () => store.openSequence(id), "Edit its own timeline");
      (open as HTMLButtonElement).disabled = id === store.state.sequence;
      actions.appendChild(open);
    }
    // The same gesture for the other kinds of entry that open on their own.
    if (isEntryTab(p, id)) {
      const what = isOverlaySource(item.source) ? "Iterate on it in the overlay workbench" : `${itemKind(item) === "image" ? "Look at it" : "Play it"} in the viewer, on its own`;
      const open = button("Open", () => store.openEntry(id), what);
      (open as HTMLButtonElement).disabled = id === store.state.tab;
      actions.appendChild(open);
    }
    if (isOverlaySource(item.source)) {
      actions.appendChild(button("Preview", () => void renderPreview({ bins: [id] }), "Render its animated preview with these vars"));
      actions.appendChild(button("Save as blueprint", () => void saveAsBlueprint(id), "Copy this overlay into your library, to reuse in other projects"));
    }
    const remove = button("Remove from bin", () => {
      if (places) return toast(`${id} is placed by ${plural(places, "clip")}`, "err");
      const kids = childrenOf(p, id).length;
      if (kids) return toast(`${id} has ${plural(kids, "entry", "entries")} listed under it`, "err");
      if (id === p.master) return toast("The master sequence cannot be removed", "err");
      void store.patch([{ op: "remove", path: bp }], `remove ${id}`);
    });
    remove.title = places ? "Remove its clips first" : "Delete this entry";
    actions.appendChild(remove);
    this.host.appendChild(actions);
    const past = historyFor([id], "this entry");
    this.off.push(past.off);
    this.host.appendChild(past.box);
  }

  /** The `source` half of an entry — the part that differs per kind. */
  private renderSource(p: Project, id: string, item: BinItem, bp: string) {
    if (isFileSource(item.source)) {
      this.host.appendChild(field("Path", textInput(item.source.path, (v) => store.patch([{ op: "replace", path: `${bp}/source/path`, value: v }], `path ${id}`))));
      const facts = mediaFacts(item.meta);
      if (facts) this.host.appendChild(field("Probed", el("div", "mono muted", facts)));
      return;
    }
    if (isColorSource(item.source)) {
      const c = el("input");
      c.type = "color";
      c.value = String(item.source.color).slice(0, 7);
      c.onchange = () => store.patch([{ op: "replace", path: `${bp}/source/color`, value: c.value }], `colour ${id}`);
      this.host.appendChild(field("Colour", c));
      return;
    }
    if (isFolderSource(item.source)) {
      this.host.appendChild(field("Contents", el("div", "mono muted", plural(childrenOf(p, id).length, "entry", "entries"))));
      return;
    }
    if (isSequenceSource(item.source)) {
      const clips = item.source.tracks.reduce((a, t) => a + t.clips.length, 0);
      this.host.appendChild(field("Length", el("div", "mono muted", `${seqDuration(p, id).toFixed(2)}s`)));
      this.host.appendChild(field("Contents", el("div", "mono muted", `${plural(item.source.tracks.length, "track")} · ${plural(clips, "clip")}`)));
      if (id === p.master) this.host.appendChild(el("p", "muted tiny", "This is the master — it is what renders."));
      return;
    }
    if (!isOverlaySource(item.source)) return;

    const source = item.source;
    const manifest = manifestOf(source);
    this.host.appendChild(field("Folder", el("div", "mono muted", source.overlay)));
    // A shared folder: editing it changes every entry on it.
    const others = sharersOf(source.overlay).filter((x) => x !== id);
    if (others.length) {
      const note = el("p", "muted tiny", `Shares its folder with ${others.join(", ")}: editing the HTML changes ${others.length > 1 ? "them all" : "both"}.`);
      this.host.appendChild(note);
      this.host.appendChild(row("actions", button("Make unique", () => void makeOverlayUnique(id), "Copy the folder so this entry has its own, and edit it alone")));
    }
    if (!manifest) {
      this.host.appendChild(el("p", "muted", overlayError(source)));
      return;
    }
    // Absent until Preview has rendered one.
    this.host.appendChild(previewImg(`/api/bin/${encodeURIComponent(id)}`));

    const vars = source.vars ?? {};
    const setVar = (k: string, v: unknown) => store.patch([{ op: "add", path: `${bp}/source/vars`, value: { ...vars, [k]: v } }], `var ${k}`);
    const value = (k: string) => (k in vars ? vars[k] : manifest.vars[k]?.default);
    // A control whose `when` does not hold is left out: an outline colour means nothing
    // with the outline off. The value stays, and comes back with the switch.
    const shown = (spec: VarSpec) => Object.entries(spec.when ?? {}).every(([k, want]) => (Array.isArray(want) ? want.includes(value(k)) : want === value(k)));
    for (const [group, entries] of varGroups(manifest.vars)) {
      const host = group ? varSection(this.host, group) : this.host;
      for (const [k, spec] of entries) {
        if (!shown(spec)) continue;
        const f = field(spec.label ?? k, varInput(spec, value(k), (v) => setVar(k, v)));
        if (spec.description) f.title = spec.description;
        host.appendChild(f);
      }
    }
  }

  /**
   * The open sequence's own effects, when nothing is selected: they act on its mix, so a
   * gain and a limiter here are the master stage when the master is open. Keys count in the
   * sequence's own seconds, which is what the playhead is while it is open.
   */
  private renderSequenceEffects() {
    const p = store.state.project;
    const id = store.state.sequence;
    const seq = p && sequenceOf(p, id);
    if (!seq) return;
    const path = `${seqPath(id)}/effects`;
    const span = { start: 0, end: seqDuration(p, id) };
    const name = p.bin[id]?.name || id;
    const carries = sequenceStreams(p, id);
    this.renderEffects({
      key: `seq:${id}`,
      what: "the sequence",
      effects: seq.effects ?? [],
      path,
      carries,
      offers: (type) => SEQUENCE_EFFECTS.includes(type) && actsOn(type, carries),
      anim: (ei) => {
        const e = seq.effects?.[ei];
        return e ? new Animation(e, span, (next, note) => void store.patch([{ op: "replace", path: `${path}/${ei}`, value: next }], `${e.type} ${note}`)) : null;
      },
      add: (type) => EFFECTS[type]!.add,
    }, `${id === p.master ? "Master" : "Sequence"} “${name}” — on its mix`);
  }

  private renderEffects(owner: EffectOwner, heading = "Effects") {
    this.host.appendChild(el("h3", "", heading));
    // Everything that follows the playhead without the panel being rebuilt for it.
    const followers: (() => void)[] = [];
    owner.effects.forEach((e, ei) => {
      const id = `${owner.key}/${ei}`;
      const box = el("div", `effect${closedEffects.has(id) ? " closed" : ""}`);
      // The fields live in a box of their own so that folding is one class on the effect,
      // and so the head — the one thing a closed effect still shows — is never part of it.
      const body = el("div", "effect-body");
      /** This effect's followers. They are the outer list's, but held here first: a closed
       *  effect follows nothing, and this is what makes that one check rather than one per
       *  field. */
      const mine: (() => void)[] = [];

      const head = el("div", "head");
      const spec = EFFECTS[e.type] ?? { add: e };
      const name = title(e.type);
      const lower = name.toLowerCase();
      const disclose = button(icon("chevron-right", 11), () => fold(), `Show or hide this ${lower}'s settings`, "disclose");
      disclose.append(name);
      disclose.setAttribute("aria-expanded", String(!closedEffects.has(id)));
      const fold = () => {
        const opening = closedEffects.delete(id);
        if (!opening) closedEffects.add(id);
        box.classList.toggle("closed", !opening);
        disclose.setAttribute("aria-expanded", String(opening));
        // Catch up on the way open: a closed effect's keyed fields have been sitting out
        // the playhead, so they read whatever they did when it was shut.
        if (opening) for (const f of mine) f();
      };
      // Not offered for this clip, but here — by patch, or because the clip moved to a
      // track that takes away what it acted on. The validator warns too; this says it where
      // the effect is, so it does not read as broken.
      if (!actsOn(e.type, owner.carries)) {
        box.classList.add("inert");
        disclose.append(el("span", "inert-note", "no effect here"));
        disclose.title = `This ${lower} does nothing: ${owner.what} ${streamsOf(e.type).video ? "shows no picture" : "has no sound"}`;
      }
      head.append(disclose);
      const remove = button("×", () => store.patch([{ op: "remove", path: `${owner.path}/${ei}` }], `remove ${e.type}`), `Remove this ${lower}`, "fx-remove");
      box.append(head, body);
      const rec = e as unknown as Record<string, unknown>;
      const defaults = spec.add as unknown as Record<string, unknown>;
      // A key may name one part of an object field — a grade's `lift.r` — which reads and
      // writes that part, keeping the rest of the object.
      const cur = (key: string): unknown => {
        const [k, part] = key.split(".");
        const v = rec[k] ?? defaults[k];
        return part === undefined ? v : (v as Record<string, unknown> | undefined)?.[part];
      };
      const replace = (value: unknown, note: string) => void store.patch([{ op: "replace", path: `${owner.path}/${ei}`, value }], `${e.type} ${note}`);
      const set = (key: string, v: unknown) => {
        const [k, part] = key.split(".");
        replace({ ...e, [k]: part === undefined ? v : { ...(cur(k) as object | undefined), [part]: v } }, `${key}`);
      };

      // Keyframes, when this kind takes them: an animatable field reads through the one
      // sampler (which is the static value until there are keys), and once there are keys
      // it writes the value *at the playhead* rather than a static one.
      const anim = ANIMATABLE[e.type] ? owner.anim(ei) : null;
      const animatable = (key: string) => !!anim && anim.fields.includes(key);
      const timed = (key: string) => animatable(key) && anim!.animated;
      // Auto-key: with it on, a change to any animatable parameter is a key at the
      // playhead, whether or not the effect had keys — Godot's record button. Read when
      // the write happens, not when the row is built, so the toggle needs no re-render.
      const recording = (key: string) => store.state.autoKey && animatable(key) && anim!.inside;
      const ctx: PropCtx = {
        read: (key) => (animatable(key) ? anim!.at(key) : cur(key)),
        write: (key, v) => (timed(key) || recording(key) ? anim!.setAt(key, Number(v)) : set(key, v)),
        // A keyed field follows the playhead: one subscription per effect updates the
        // values in place, because re-rendering this panel 25 times a second is what
        // `store.select` exists to prevent.
        timed: (key) => (timed(key) ? { readOnly: !anim!.inside, live: (set) => mine.push(() => set(anim!.at(key))) } : {}),
      };
      // The toggle is in the head, so a keyframed effect says so while folded too.
      if (anim) head.append(keyToggle(anim));
      head.append(remove);
      for (const p of spec.props ?? []) body.appendChild(propRow(p, ctx));
      if (anim?.animated) body.appendChild(keyRow(anim, id, (fn) => mine.push(fn)));
      if (mine.length) followers.push(() => { if (!closedEffects.has(id)) for (const f of mine) f(); });
      this.host.appendChild(box);
    });
    // Only what can act on this clip: a volume on a still, or a picture fade on a song,
    // would be accepted and do nothing, and that reads as a broken effect.
    const add = el("select");
    const offered = (Object.keys(EFFECTS) as Effect["type"][]).filter((type) => owner.offers(type));
    options(add, offered.map((value) => ({ value, label: title(value).toLowerCase() })), undefined, "+ add effect…");
    add.onchange = () => {
      const kind = add.value as Effect["type"] | "";
      add.value = "";
      if (kind) store.patch([{ op: "add", path: `${owner.path}/-`, value: owner.add(kind) }], `add ${kind}`);
    };
    this.host.appendChild(add);
    if (followers.length) {
      // Unseen, the values wait, and catch up when the panel is shown.
      const follow = panels.lazily("inspector", () => { for (const f of followers) f(); });
      this.off.push(store.select((s) => [s.playhead], follow.run), follow.stop);
    }
  }

  /** Presets expand to ordinary effects; their params are edited here before applying. */
  private presetPicker(path: string): HTMLElement {
    const wrap = el("div");
    const preset = el("select");
    preset.style.marginTop = "6px";
    const params = el("div");
    presetsPromise.then((ps) => options(preset, ps.map((p) => ({ value: p.id, label: p.name, title: p.description })), undefined, "+ apply preset…"));
    preset.onchange = async () => {
      params.innerHTML = "";
      const p = (await presetsPromise).find((x) => x.id === preset.value);
      if (!p) return;
      const values: Record<string, number> = {};
      for (const [k, spec] of Object.entries(p.params)) {
        values[k] = Number(spec.default);
        params.appendChild(field(spec.label ?? k, varInput(spec, values[k], (v) => { values[k] = Number(v); })));
      }
      params.appendChild(button("Apply", async () => {
        try {
          const { width, height } = store.state.project!;
          const ops: PatchOp[] = expandPreset(p, { width, height, ...values }).map((e) => ({ op: "add", path: `${path}/effects/-`, value: e }));
          if (await store.patch(ops, `preset ${p.id}`)) { preset.value = ""; params.innerHTML = ""; }
        } catch (e) {
          toast((e as Error).message, "err");
        }
      }));
    };
    wrap.append(preset, params);
    return wrap;
  }
}

// ---- inputs --------------------------------------------------------------------

/** A button that reads as a link: it navigates the panel rather than changing anything. */
const linkButton = (label: string, onClick: () => void, title = "") => button(label, onClick, title, "linkish");

function field(label: string, input: HTMLElement): HTMLDivElement {
  const row = el("div", "field");
  row.append(el("label", "", label), input);
  return row;
}

function textInput(value: string, onChange: (v: string) => void): HTMLInputElement {
  const i = el("input");
  i.type = "text";
  i.value = value;
  i.onchange = () => onChange(i.value);
  return i;
}

function jsonInput(value: unknown, onChange: (v: unknown) => void): HTMLTextAreaElement {
  const t = el("textarea", "wide");
  t.value = JSON.stringify(value, null, 1);
  t.onchange = () => { try { onChange(JSON.parse(t.value)); t.style.borderColor = ""; } catch { t.style.borderColor = "var(--danger)"; } };
  return t;
}

/** Subtitle cues as editable rows — start, end, text, remove — plus a row to add one.
 *  Every change commits the whole list, which is one var patch like any other. */
function cuesInput(cues: Cue[], onChange: (v: Cue[]) => void): HTMLElement {
  const wrap = el("div", "cues wide");
  const change = (i: number, patch: Partial<Cue>) => onChange(cues.map((c, j) => (j === i ? { ...c, ...patch } : c)));
  const drop = (i: number) => onChange(cues.filter((_, j) => j !== i));
  cues.forEach((c, i) => {
    wrap.appendChild(row("cue-row",
      numberField(c.start, (v) => change(i, { start: v }), { step: 0.01 }),
      numberField(c.end, (v) => change(i, { end: v }), { step: 0.01 }),
      textInput(c.text, (v) => change(i, { text: v })),
      button("×", () => drop(i), "Remove this cue"),
    ));
  });
  const end = cues.at(-1)?.end ?? 0;
  wrap.appendChild(button("+ cue", () => onChange([...cues, { start: end, end: end + 2, text: "" }])));
  return wrap;
}

/** A manifest's vars by `group`, the ungrouped first, then each group where its first var
 *  is; within a group, in the order written. */
function varGroups(vars: Record<string, VarSpec>): [string, [string, VarSpec][]][] {
  const groups = new Map<string, [string, VarSpec][]>([["", []]]);
  for (const [k, spec] of Object.entries(vars)) {
    const g = spec.group ?? "";
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g)!.push([k, spec]);
  }
  return [...groups].filter(([, entries]) => entries.length);
}

/** Folded var sections, by label: a Box section folded on one title stays folded on the
 *  next, which is what someone working through a set of them wants. */
const closedSections = new Set<string>();

/** A foldable section of vars under `host`; returns where its fields go. */
function varSection(host: HTMLElement, label: string): HTMLElement {
  const box = el("div", `effect var-group${closedSections.has(label) ? " closed" : ""}`);
  const head = el("div", "head");
  const disclose = button(icon("chevron-right", 11), () => {
    const closing = !closedSections.has(label);
    if (closing) closedSections.add(label); else closedSections.delete(label);
    box.classList.toggle("closed", closing);
    disclose.setAttribute("aria-expanded", String(!closing));
  }, `Show or hide the ${label.toLowerCase()} settings`, "disclose");
  disclose.append(label);
  disclose.setAttribute("aria-expanded", String(!closedSections.has(label)));
  head.appendChild(disclose);
  const body = el("div", "effect-body");
  box.append(head, body);
  host.appendChild(box);
  return body;
}

/**
 * A font var: the installed families, the project's first, shown in the chosen face; and
 * a way to add one. A family the entry names that is not installed stays selectable as
 * itself, marked, rather than silently becoming another.
 */
function fontInput(cur: string, onChange: (v: string) => void): HTMLElement {
  const fonts = store.state.fonts;
  const s = el("select");
  const group = (label: string, families: FontFamily[]) => {
    if (!families.length) return;
    const g = el("optgroup");
    g.label = label;
    for (const f of families) {
      const o = el("option", "", f.name);
      o.value = f.name;
      o.style.fontFamily = `"${f.name}", system-ui`;
      // Text in a script the family lacks is drawn by whatever this machine has.
      o.title = `Sets ${f.scripts.join(", ") || "no script kuma checks for"}`;
      g.appendChild(o);
    }
    s.appendChild(g);
  };
  if (cur && !fonts.some((f) => f.name === cur)) {
    const o = el("option", "", `${cur} (not installed)`);
    o.value = cur;
    s.appendChild(o);
  }
  group("This project", fonts.filter((f) => f.origin === "project"));
  group("Library", fonts.filter((f) => f.origin !== "project"));
  s.value = cur;
  s.style.fontFamily = `"${cur}", system-ui`;
  s.onchange = () => onChange(s.value);
  const add = button("+", () => void addFonts(onChange), "Add font files (.ttf, .otf, .woff, .woff2, or a .ttc collection) to this project or to every project");
  return row("row", s, add);
}

/**
 * Add fonts by path, as media is added: the app's picker, or a typed path in a browser tab,
 * which cannot say where a file is. Ask where they go, add them, use the first one's family.
 */
export async function addFonts(use: (family: string) => void = () => {}): Promise<void> {
  const app = desktop();
  const typed = el("input");
  typed.type = "text";
  typed.placeholder = "fonts/MyFont-Regular.ttf or /abs/path.otf";
  let paths: string[] = [];
  if (app) {
    paths = await app.pickFonts().catch((e) => { toast(`Cannot open the file picker: ${(e as Error).message}`, "err"); return []; });
    if (!paths.length) return;
  }
  const where = selectInput([
    { value: "project", label: "This project", title: "Into the project's fonts/: it travels with the project" },
    { value: "shared", label: "Every project", title: "Into your library's fonts/, beside the starter fonts" },
  ], "project", () => {});
  const ok = await askForm({
    title: paths.length > 1 ? `Add ${paths.length} fonts` : "Add a font",
    note: "A .ttf or .otf file is copied in; a .ttc or .otc collection is split into its faces, and a .woff or .woff2 web font is unwrapped into the font it holds. A family's weights and italics are separate files in most fonts: add them together. A font in the project goes wherever the project goes; one in your library is there for every project on this machine.",
    rows: [...(app ? [] : [{ label: "File", input: typed }]), { label: "For", input: where }],
    ok: "Add",
  });
  if (!ok) return;
  if (!app) paths = typed.value.trim() ? [typed.value.trim()] : [];
  let first: string | undefined;
  for (const p of paths) {
    try {
      first ??= (await api.addFont(p, where.value === "shared")).family;
    } catch (e) {
      toast(`${p}: ${(e as Error).message}`, "err", 8000);
    }
  }
  await store.reloadFonts();
  if (first) {
    toast(`Added ${first}`, "ok");
    use(first);
  }
}

function varInput(spec: VarSpec, cur: unknown, onChange: (v: unknown) => void): HTMLElement {
  switch (spec.type) {
    case "font":
      return fontInput(String(cur ?? ""), onChange);
    case "number":
      // A var that named its bounds gets the slider; one that did not is a bare number,
      // rather than a slider over a range nobody declared.
      return spec.min !== undefined && spec.max !== undefined
        ? sliderField(Number(cur), onChange, { min: spec.min, max: spec.max, step: spec.step ?? 0.01, soft: true })
        : numberField(Number(cur), onChange, { step: spec.step ?? 0.01, min: spec.min, max: spec.max });
    case "color": {
      const c = el("input");
      c.type = "color";
      c.value = String(cur).slice(0, 7);
      c.onchange = () => onChange(c.value);
      return c;
    }
    case "boolean":
      return checkbox(Boolean(cur), onChange);
    case "enum":
      return selectInput((spec.options ?? []).map((value) => ({ value })), String(cur), onChange);
    case "point": {
      // The same row a transform's position is: a point is a point wherever it turns up.
      const p = (cur ?? { x: 0.5, y: 0.5 }) as { x: number; y: number };
      return vectorField([
        { tag: "X", axis: "x", step: 0.01, value: p.x, commit: (v) => onChange({ x: v, y: p.y }) },
        { tag: "Y", axis: "y", step: 0.01, value: p.y, commit: (v) => onChange({ x: p.x, y: v }) },
      ]);
    }
    case "text": {
      const t = el("textarea", "wide");
      t.value = String(cur ?? "");
      t.onchange = () => onChange(t.value);
      return t;
    }
    case "cues":
      return cuesInput((Array.isArray(cur) ? cur : []) as Cue[], onChange);
    default:
      return textInput(String(cur ?? ""), onChange);
  }
}
