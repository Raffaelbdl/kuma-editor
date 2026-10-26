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
import { binName, fmtTime, isEntryTab, store, toast } from "../store";
import type { BinItem, Clip, ColorReference, Cue, Ease, Effect, FontFamily, Grade, Keyframe, Mask, PatchOp, Project, Streams, Transition, VarSpec } from "../types";
import { pickRegion } from "../regionpick";
import { previewGrade, settle, showBefore } from "../gradepreview";
import { blueprintActions, copyEffects, makeOverlayUnique, matchSelectedToReference, measureMatch, removeTransition, setTransitionDuration, setTransitionKind, updateTransition, manifestOf, moveBinItem, overlayError, placeBinItem, previewImg, renderPreview, saveAsBlueprint, sharersOf } from "../edits";
import { Animation, animationOf } from "../keyframes";
import { historyFor } from "./history";
import { icon, svgEl } from "../icons";
import type { Axis, NumberOpts } from "../props";
import { keyStrip, numberField, showNumber, sliderField, vectorField } from "../props";
import { colorField, wheelField } from "../colorfield";
import { curveField } from "../curvefield";
import type { CurveKind, CurvePoint } from "../curve";
import { LUMAS, MASK_KEYED, maskAt, SEQUENCE_EFFECTS, sourceAfter, actsOn, ANIMATABLE, binItem, binPath, canPlace, canTimeWarp, childrenOf, clipStreams, covers, expandPreset, hasAudio, isColorSource, isFileSource, isFolderSource, isOverlaySource, isSequenceSource, isTimedMedia, itemKind, mediaFacts, placementsOf, seqDuration, seqPath, sequenceOf, sequenceStreams, streamsOf, wouldNest } from "../types";

const presetsPromise = api.effectPresets().catch(() => []);

/** What one field of an effect looks like. The shapes come from `props.ts` — a number you
 *  drag, a vector wearing its axis colours, a bounded amount with a slider — and this says
 *  which shape each field is. Nothing else in the panel needs to know. */
type PropSpec =
  | ({ kind: "num"; key: string; label?: string } & NumberOpts)
  | ({ kind: "slider"; key: string; label?: string; min: number; max: number; /** The range is a suggestion, not a limit: the field may be typed past it. */ soft?: boolean } & NumberOpts)
  | { kind: "vec"; label: string; parts: ({ key: string } & NumberOpts)[] }
  | { kind: "enum"; key: string; label?: string; /** The values, or what reads them when the panel is built. */ options: string[] | (() => { value: string; label?: string }[]); /** Offer none, which leaves the field out. */ optional?: string; title?: string; /** Read the options again, as the list is opened: they are files, which nothing announces. */ refresh?: () => void }
  | { kind: "text"; key: string; label?: string; /** Empty means the field is left out, not an empty string. */ optional?: boolean }
  | { kind: "json"; key: string; label?: string }
  /** Several curves sharing one plot, a tab each: the one picked is edited. */
  | { kind: "curves"; label: string; curves: CurveTab[] }
  /** Per-channel offsets as colour wheels side by side (`colorfield.ts`), one per key. */
  | { kind: "wheels"; label: string; keys: { key: string; title: string }[] }
  /** A grade's white balance and exposure, measured into place from the picture: a clip's
   *  only, since it measures the clip (a grade never sits on a sequence). */
  | { kind: "match"; label: string }
  /** Where the effect acts: the whole picture, or inside a shape (`Mask`). */
  | { kind: "mask"; label: string };

/** Per effect kind: the value `+ add effect` inserts, and the properties it shows, in
 *  order. Its name is its type, spaced (`title`). Adding a kind is one row here plus the
 *  Rust model (and its streams in `types.ts`); one the daemon knows and this table does not
 *  renders with no fields. A kind in `ANIMATABLE` gets the keyframe toggle in its head, and
 *  the keyframe row once it has keys. Units are the model's, and the tooltips say so where
 *  it is not obvious. */
interface EffectSpec {
  add: Effect;
  props?: PropSpec[];
  /** Shown on the frame while a control is dragged, before the edit (`gradepreview.ts`). */
  live?: boolean;
}
const EASES: Ease[] = ["linear", "hold", "smooth"];
/** A fade's two lengths, whichever stream it fades. */
const fadeProps = (into: string, outOf: string): PropSpec[] => [{ kind: "vec", label: "Duration", parts: [
  { key: "in", tag: "In", step: 0.1, min: 0, suffix: "s", title: `Seconds to ${into}` },
  { key: "out", tag: "Out", step: 0.1, min: 0, suffix: "s", title: `Seconds to ${outOf}` },
] }];
/** One row of a grade's per-channel numbers — R, G and B, neutral at 0, addressed as
 *  `lift.r` — under its wheel. */
const CHANNELS = [["r", "x", "red"], ["g", "y", "green"], ["b", "z", "blue"]] as const;
const channels = (key: string, what: string): PropSpec => ({ kind: "vec", label: capitalize(key), parts: CHANNELS.map(([ch, axis, name]) => (
  { key: `${key}.${ch}`, tag: ch.toUpperCase(), axis, step: 0.01, min: -1, max: 1, title: `${what}, on ${name}` }
)) });
/** One tab of a `curves` row. */
interface CurveTab { key: string; tab: string; kind: CurveKind; channel?: "r" | "g" | "b"; axes?: [string, string]; title: string }
const ADD_POINTS = "press to add a point, drag to move it, double-click to take it out";
/** The grade's curves, in the order they apply: the tone curves, then step 7's. */
const GRADE_CURVES: CurveTab[] = [
  { key: "curve", tab: "Luma", kind: "tone", title: `The tone curve, on every channel: ${ADD_POINTS}` },
  { key: "curve_r", tab: "R", kind: "tone", channel: "r", title: `Red's own tone curve, after the first: ${ADD_POINTS}` },
  { key: "curve_g", tab: "G", kind: "tone", channel: "g", title: `Green's own tone curve, after the first: ${ADD_POINTS}` },
  { key: "curve_b", tab: "B", kind: "tone", channel: "b", title: `Blue's own tone curve, after the first: ${ADD_POINTS}` },
  { key: "hue_sat", tab: "Hue·Sat", kind: "hue", axes: ["hue", "sat"], title: `Saturation by hue: up for more of that hue, down to grey it. It wraps round: ${ADD_POINTS}` },
  { key: "hue_hue", tab: "Hue·Hue", kind: "hue", axes: ["hue", "turn"], title: `Hue by hue: up or down turns that hue towards its neighbours, half way round at the edge. It wraps round: ${ADD_POINTS}` },
  { key: "hue_luma", tab: "Hue·Luma", kind: "hue", axes: ["hue", "luma"], title: `Luma by hue: brighter or darker for that hue, the more the more saturated, so greys stay. It wraps round: ${ADD_POINTS}` },
  { key: "luma_sat", tab: "Luma·Sat", kind: "amount", axes: ["luma", "sat"], title: `Saturation by luma, black on the left: up for more, down to grey (say, the shadows). ${capitalize(ADD_POINTS)}` },
  { key: "sat_sat", tab: "Sat·Sat", kind: "amount", axes: ["sat", "sat"], title: `Saturation by saturation, grey on the left: lift the dull colours, calm the loud ones. ${capitalize(ADD_POINTS)}` },
];
/** The camera logs a grade can name: files the daemon lists (`/api/logs`). */
const cameraLogs = () => store.state.logs.map((l) => ({ value: l.id, label: l.name }));
/** Which curve tab is open: view state, kept across the rebuild every edit causes. */
let curveTab = GRADE_CURVES[0].key;
const tone = (key: string, what: string): PropSpec => ({ kind: "slider", key, min: -1, max: 1, step: 0.01, title: `${what}: up raises it, by a quarter of white at 1, on every channel alike` });
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
  blur: { add: { type: "blur", radius: 4 }, props: [{ kind: "num", key: "radius", step: 0.5, min: 0, suffix: "px" }, { kind: "mask", label: "Where" }] },
  color: {
    add: { type: "color", ...ANIMATABLE.color },
    // Neutral is 0, 1, 1, 1. The ranges are what the eye uses; `soft` lets a typed value
    // go past them, because the filter underneath takes more than the slider offers.
    props: [
      { kind: "slider", key: "brightness", min: -1, max: 1, step: 0.01, soft: true },
      { kind: "slider", key: "contrast", min: 0, max: 3, step: 0.01, soft: true },
      { kind: "slider", key: "saturation", min: 0, max: 3, step: 0.01, soft: true },
      { kind: "slider", key: "gamma", min: 0.1, max: 4, step: 0.01, soft: true },
      { kind: "mask", label: "Where" },
    ],
  },
  // Compiled as one LUT, in this order (`kuma_core::grade`), so the rows read top to bottom
  // the way a colour goes through it. Nothing here animates: a grade is the clip's, and
  // grading two halves differently is a split.
  grade: {
    live: true,
    add: { type: "grade", temperature: 0, tint: 0, exposure: 0, contrast: 1, curve: [], saturation: 1, look_amount: 1 },
    props: [
      { kind: "enum", key: "log", label: "Camera log", options: cameraLogs, optional: "none (not log footage)", refresh: () => void store.reloadLogs(),
        title: "The log the footage was shot in, converted to Rec. 709 first: what the maker's LUT does, without the file. Use this or an input LUT" },
      { kind: "text", key: "input_lut", label: "Input LUT", optional: true },
      { kind: "slider", key: "temperature", min: -2, max: 2, step: 0.01, title: "Warmer above 0, cooler below: red and blue a stop apart at ±1, and tungsten against daylight is about 1.5" },
      { kind: "slider", key: "tint", min: -1, max: 1, step: 0.01, title: "Towards magenta above 0, green below" },
      { kind: "slider", key: "exposure", min: -5, max: 5, step: 0.05, suffix: "EV", title: "Stops of light: 1 doubles it" },
      // The wheels first, for the colour, and the channel rows under them for exact numbers.
      { kind: "wheels", label: "Wheels", keys: [
        { key: "lift", title: "Lift: drag towards a colour to tint the shadows; white stays" },
        { key: "gamma", title: "Gamma: drag towards a colour to tint the midtones; black and white stay" },
        { key: "gain", title: "Gain: drag towards a colour to tint the highlights; black stays" },
      ] },
      channels("lift", "Raises the shadows, white stays"),
      channels("gamma", "Bends the midtones, black and white stay"),
      channels("gain", "Scales the highlights, black stays"),
      { kind: "slider", key: "contrast", min: 0, max: 3, step: 0.01, title: "About mid grey: 1 leaves it" },
      tone("blacks", "The darkest tones"),
      tone("shadows", "The shadows, a third of the way up"),
      tone("highlights", "The highlights, two thirds of the way up"),
      tone("whites", "The brightest tones"),
      { kind: "curves", label: "Curves", curves: GRADE_CURVES },
      { kind: "slider", key: "saturation", min: 0, max: 3, step: 0.01, title: "1 leaves it, 0 is black and white" },
      { kind: "slider", key: "vibrance", min: -1, max: 1, step: 0.01, title: "Saturation where there is little, leaving vivid colours (and faces) alone: 1 doubles a dull colour's, -1 greys it" },
      { kind: "text", key: "look", label: "Look LUT", optional: true },
      { kind: "slider", key: "look_amount", label: "Look amount", min: 0, max: 1, step: 0.01 },
      { kind: "mask", label: "Where" },
      { kind: "match", label: "Match" },
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
  /** The clip, when the owner is one. */
  clip?: string;
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
  /** The clip the effect is on, when it is on one: what a match measures. */
  clip?: string;
  /** Which of the owner's effects this is. */
  effect: number;
  /** Write several fields of the effect as one edit. */
  merge: (fields: Record<string, unknown>) => void;
  /** Show what a value would do while it is dragged, for a kind that can (`live`). */
  preview?: (key: string, v: unknown) => void;
  /** Add another effect of this kind to the owner, with `fields` over what one starts as. */
  another: (fields: Record<string, unknown>) => void;
  /** The effect's timing on its clip — the playhead in its time — for a kind that animates. */
  anim: Animation | null;
  /** Call `fn` whenever the playhead moves, while the panel shows this effect. */
  follow: (fn: () => void) => void;
}

/** One property row, whatever shape it is. What it edits is on the row: the panel styles
 *  by it, and a test can name a property rather than counting divs. */
function propRow(p: PropSpec, ctx: PropCtx): HTMLElement {
  const { read, write, timed } = ctx;
  const input = (key: string) => (ctx.preview ? { input: (v: unknown) => ctx.preview!(key, v) } : {});
  const num = (key: string) => Number(read(key) ?? 0);
  // A kind that edits one key is named by it; one that edits several, by its label.
  const label = "key" in p ? p.label ?? title(p.key) : p.label;
  let control: HTMLElement;
  switch (p.kind) {
    case "vec":
      control = vectorField(p.parts.map((part) => ({ ...part, ...timed(part.key), ...input(part.key), value: num(part.key), commit: (v: number) => write(part.key, v) })));
      break;
    case "num":
      control = numberField(num(p.key), (v) => write(p.key, v), { ...p, ...timed(p.key), ...input(p.key) });
      break;
    case "slider":
      control = sliderField(num(p.key), (v) => write(p.key, v), { ...p, ...timed(p.key), ...input(p.key) });
      break;
    case "enum":
      {
        const now = String(read(p.key) ?? "");
        const options: { value: string; label?: string }[] = typeof p.options === "function" ? p.options() : p.options.map((value) => ({ value }));
        // A value the list does not have — a log whose file is gone — is still shown, as
        // what it is, rather than read as another.
        if (now && !options.some((o) => o.value === now)) options.push({ value: now, label: `${now} (not found)` });
        control = selectInput([...(p.optional ? [{ value: "", label: p.optional }] : []), ...options], now, (v) => write(p.key, v === "" && p.optional ? undefined : v));
        if (p.title) control.title = p.title;
        if (p.refresh) control.addEventListener("focus", p.refresh);
      }
      break;
    case "text":
      control = textInput(String(read(p.key) ?? ""), (v) => write(p.key, v === "" && p.optional ? undefined : v));
      break;
    case "json":
      control = jsonInput(read(p.key) ?? null, (v) => write(p.key, v));
      break;
    case "curves": {
      const tabs = el("div", "curve-tabs");
      const host = el("div");
      const points = (key: string) => (read(key) as CurvePoint[] | undefined) ?? [];
      const show = () => {
        const c = p.curves.find((c) => c.key === curveTab) ?? p.curves[0];
        for (const b of tabs.children) b.classList.toggle("on", (b as HTMLElement).dataset.curve === c.key);
        host.replaceChildren(curveField(points(c.key), (v) => write(c.key, v.length ? v : undefined), { title: c.title, kind: c.kind, channel: c.channel, axes: c.axes, ...input(c.key) }));
      };
      for (const c of p.curves) {
        const b = button(c.tab, () => { curveTab = c.key; show(); }, c.title.split(":")[0]);
        b.dataset.curve = c.key;
        // A tab whose curve bends something says so while another is open.
        b.classList.toggle("set", points(c.key).length > 0);
        tabs.appendChild(b);
      }
      show();
      control = el("div", "curves wide");
      control.append(tabs, host);
      break;
    }
    case "mask":
      // Where and when are a placement's: an entry's effects are over its whole picture.
      control = ctx.clip ? maskControls(ctx) : el("span", "muted tiny", "The whole picture: a shape goes on a clip's own grade");
      break;
    case "match":
      // A grade is a clip's: the model offers it to nothing else.
      control = ctx.clip ? matchControls(ctx.clip, ctx.effect, ctx.merge) : el("span", "muted tiny", "Measured from a clip that places it: match there");
      break;
    case "wheels":
      control = row("wheels wide", ...p.keys.map(({ key, title: what }) => {
        const o = (read(key) ?? {}) as { r?: number; g?: number; b?: number };
        return wheelField(title(key), { r: o.r ?? 0, g: o.g ?? 0, b: o.b ?? 0 }, (v) => write(key, v), { title: what, ...input(key) });
      }));
      break;
  }
  const line = field(label, control);
  line.dataset.prop = "key" in p ? p.key : p.label.toLowerCase();
  return line;
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

/** Where focus is inside `host`, as a way to put it back once `host` is rebuilt: the
 *  element at the same place — the same child of the same child, down from `host` — when it
 *  is the same kind of element in the same property row, with its text selected as it was.
 *  Null when focus is elsewhere. */
function focusIn(host: HTMLElement): (() => void) | null {
  const at = document.activeElement;
  if (!(at instanceof HTMLElement) || at === host || !host.contains(at)) return null;
  const path: number[] = [];
  for (let n: Element = at; n !== host; n = n.parentElement!) path.unshift([...n.parentElement!.children].indexOf(n));
  const row = (e: Element) => e.closest<HTMLElement>("[data-prop]")?.dataset.prop;
  const prop = row(at);
  const range = at instanceof HTMLInputElement || at instanceof HTMLTextAreaElement ? [at.selectionStart, at.selectionEnd, at.selectionDirection] as const : null;
  return () => {
    let n: Element | undefined = host;
    for (const i of path) n = n?.children[i];
    if (!(n instanceof HTMLElement) || n.tagName !== at.tagName || row(n) !== prop) return;
    n.focus({ preventScroll: true });
    if (range && (n instanceof HTMLInputElement || n instanceof HTMLTextAreaElement) && range[0] !== null) {
      try { n.setSelectionRange(range[0], range[1], range[2] ?? undefined); } catch { /* a type with no selection */ }
    }
  };
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
    store.select((s) => [s.project, s.selection, s.manifests, s.fonts, s.logs, s.blueprints, s.sequence, playheadInClip(), s.previewStamp, s.colorReference], panels.lazily("inspector", () => this.render()).run);
  }

  /** The panel rebuilt from the document — which every edit changes, so it runs as a field
   *  commits. Tab commits one field and moves to the next, which the rebuild would throw
   *  away: for the same selection, the focus, the text selected in it and the scroll are
   *  put back where they were. */
  private render() {
    const sel = JSON.stringify(store.state.selection);
    const focus = this.shown === sel ? focusIn(this.host) : null;
    const scroll = this.shown === sel ? this.host.scrollTop : 0;
    this.build();
    this.shown = sel;
    this.host.scrollTop = scroll;
    focus?.();
  }

  /** The selection the panel last showed, to tell a rebuild of it from a new one. */
  private shown = "";

  private build() {
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
      clip: clip.id,
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
    // What the footage is — its camera log, the grade that makes it match the rest — set
    // once for every clip that places it.
    const kind = itemKind(item);
    if (kind === "video" || kind === "image") {
      this.renderEffects({
        key: `bin:${id}`,
        what: "this entry",
        effects: item.effects ?? [],
        path: `${bp}/effects`,
        carries: { video: true, audio: false },
        offers: (type) => type === "grade",
        anim: () => null,
        add: (type) => EFFECTS[type]!.add,
      }, "On the source");
    }
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
      const c = colorField(String(item.source.color), (v) => void store.patch([{ op: "replace", path: `${bp}/source/color`, value: v }], `colour ${id}`), { key: `${bp}/source/color` });
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
        const f = field(spec.label ?? k, varInput(spec, value(k), (v) => setVar(k, v), `${bp}/source/vars/${k}`));
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
      const copy = button(icon("copy", 11), () => copyEffects([e], `the ${lower}`), `Copy this ${lower}, to paste onto other clips (${keysFor(Cmd.pasteEffects) ?? "Ctrl + Alt + V"})`, "fx-copy");
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
      const replace = (value: unknown, note: string) => {
        // A live kind's preview stays up until the engine shows what was written.
        if (spec.live) settle();
        void store.patch([{ op: "replace", path: `${owner.path}/${ei}`, value }], `${e.type} ${note}`);
      };
      /** The effect with one field (or one part of one) changed. */
      const withValue = (key: string, v: unknown): Effect => {
        const [k, part] = key.split(".");
        return { ...e, [k]: part === undefined ? v : { ...(cur(k) as object | undefined), [part]: v } } as Effect;
      };
      const set = (key: string, v: unknown) => replace(withValue(key, v), key);

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
        clip: owner.clip,
        effect: ei,
        merge: (fields) => replace({ ...e, ...fields }, Object.keys(fields).join(", ")),
        anim,
        follow: (fn) => mine.push(fn),
        another: (fields) => void store.patch([{ op: "add", path: `${owner.path}/-`, value: { ...spec.add, ...fields } }], `add ${e.type}`),
        preview: spec.live && owner.clip ? (key, v) => {
          const { type: _, keyframes: __, ...grade } = withValue(key, v) as Effect & Grade;
          // A keyed light previews as it is at the playhead, the field dragged as dragged;
          // a moving mask where it is at the playhead.
          if (anim?.animated) for (const p of anim.fields) if (p !== key) (grade as Record<string, unknown>)[p] = anim.at(p);
          if (grade.mask?.keyframes?.length && anim) grade.mask = maskAt(grade.mask, anim.local);
          previewGrade(owner.clip!, ei, grade);
        } : undefined,
      };
      // The toggle is in the head, so a keyframed effect says so while folded too.
      if (anim) head.append(keyToggle(anim));
      head.append(copy, remove);
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
      // An owner with no effects may have no list yet (an entry writes none): the first is
      // the list.
      if (!kind) return;
      const value = owner.add(kind);
      void store.patch([owner.effects.length ? { op: "add", path: `${owner.path}/-`, value } : { op: "add", path: owner.path, value: [value] }], `add ${kind}`);
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

/** A mask a shape starts as: the middle quarter of the frame, edged by a twentieth of its
 *  shorter side. */
function startingMask(shape: "ellipse" | "rectangle"): Mask {
  const { width: W, height: H } = store.state.project!;
  return { shape, x: Math.round(W / 4), y: Math.round(H / 4), w: Math.round(W / 2), h: Math.round(H / 2), feather: Math.round(Math.min(W, H) / 20) };
}

/** Where an effect acts: the whole picture, or a shape — placed by its numbers or drawn on
 *  the frame — with another window of the same kind one click away. */
function maskControls(ctx: PropCtx): HTMLElement {
  const m = ctx.read("mask") as Mask | undefined;
  const wrap = el("div", "mask-field wide");
  const shapes = [{ value: "", label: "the whole picture" }, { value: "ellipse", label: "inside an ellipse" }, { value: "rectangle", label: "inside a rectangle" }];
  const shape = selectInput(shapes, m ? m.shape ?? "ellipse" : "", (v) => ctx.write("mask", v ? { ...(m ?? startingMask(v as "ellipse")), shape: v } : undefined));
  shape.title = "Where this effect acts: a shape keeps the rest of the picture as it was";
  const more = button("+ window", () => ctx.another({ mask: startingMask("ellipse") }), "Add another of this effect, inside a shape of its own: a second window");
  wrap.appendChild(row("mask-head", shape, more));
  if (!m) return wrap;

  // A moving mask: its fields read and write the shape at the playhead, a key there made
  // when there is none, and outside the clip there is no "now" to write to.
  const a = ctx.anim;
  const keys = m.keyframes ?? [];
  const keyed = keys.length > 0 && !!a;
  const now = () => maskAt(m, a?.local ?? 0);
  const keyIndex = () => (a ? keys.findIndex((k) => Math.abs(k.t - a.local) < store.halfFrame) : -1);
  /** The keys, copied, with `fields` in the key at the playhead — made, in its place in
   *  time and holding the shape there, when there is none. */
  const keyedHere = (at: Animation, fields: Partial<Mask>): Keyframe[] => {
    const ks = keys.map((k) => ({ ...k }));
    let i = keyIndex();
    if (i < 0) {
      const cur = now();
      const key: Keyframe = { t: at.clampT(at.local) };
      for (const f of MASK_KEYED) key[f] = cur[f] ?? 0;
      i = ks.findIndex((k) => k.t > key.t);
      if (i < 0) i = ks.length;
      ks.splice(i, 0, key);
    }
    Object.assign(ks[i], fields);
    return ks;
  };
  /** `fields` written: into the key at the playhead when the mask moves, else static. */
  const setShape = (fields: Partial<Mask>) => ctx.write("mask", keyed ? { ...m, keyframes: keyedHere(a!, fields) } : { ...m, ...fields });
  const live = (f: (typeof MASK_KEYED)[number]) => ({
    readOnly: keyed && !a!.inside,
    live: keyed ? (set: (v: number) => void) => ctx.follow(() => set(now()[f] ?? 0)) : undefined,
    // Dragged, the shape previews where it would be: at the playhead, with this field moved.
    input: ctx.preview ? (v: unknown) => ctx.preview!("mask", { ...now(), keyframes: undefined, [f]: Number(v) }) : undefined,
  });
  const part = (f: "x" | "y" | "w" | "h", tag: string, axis: Axis, title: string) => (
    { key: `mask.${f}`, tag, axis, step: 1, suffix: "px", title, value: now()[f], commit: (v: number) => setShape({ [f]: v }), ...live(f) });
  // Two rows: composition pixels run to four digits, and four fields abreast clip them.
  wrap.appendChild(vectorField([part("x", "X", "x", "The shape's left edge, in composition pixels"), part("y", "Y", "y", "Its top edge")]));
  wrap.appendChild(vectorField([part("w", "W", "z", "Its width"), part("h", "H", "w", "Its height")]));
  const turn = numberField(now().rotation ?? 0, (v) => setShape({ rotation: v }), { step: 1, suffix: "°", title: "Turned clockwise about its centre", ...live("rotation") });
  const feather = numberField(m.feather ?? 0, (v) => ctx.write("mask.feather", v), { step: 1, min: 0, suffix: "px", title: "Pixels of soft edge, centred on the shape's edge" });
  const invert = el("label", "mask-invert");
  invert.append(checkbox(!!m.invert, (v) => ctx.write("mask.invert", v || undefined)), document.createTextNode("outside"));
  invert.title = "Act outside the shape instead of inside";
  const draw = button("Draw", async () => {
    const r = await pickRegion("Where the effect acts");
    if (r) setShape(r);
  }, keyed ? "Drag the shape's rectangle over the picture: a key here" : "Drag the shape's rectangle over the picture");
  wrap.appendChild(row("mask-more", turn, feather, invert, draw));

  // Keys: one where the playhead is, holding the shape it has there, or that one taken out.
  if (a) {
    const add = button("+ key", () => {
      const cur = now();
      ctx.write("mask", { ...m, keyframes: keyedHere(a, Object.fromEntries(MASK_KEYED.map((f) => [f, cur[f] ?? 0]))) });
    }, "Key the shape where the playhead is: between keys it moves in a straight line");
    const del = button("− key", () => {
      const i = keyIndex();
      if (i < 0) return;
      const ks = keys.filter((_, j) => j !== i);
      // The last key gone, the mask holds still where it was.
      ctx.write("mask", ks.length ? { ...m, keyframes: ks } : { ...m, ...now(), keyframes: undefined });
    }, "Take out the key at the playhead");
    add.disabled = !a.inside;
    del.disabled = keyIndex() < 0;
    ctx.follow(() => { add.disabled = !a.inside; del.disabled = keyIndex() < 0; });
    const said = el("span", "muted tiny", keys.length ? plural(keys.length, "key") : "still");
    wrap.appendChild(row("mask-keys", add, del, said));
  }
  return wrap;
}

/** What a match writes: the grade's white balance and exposure, the rest of it kept. */
type Light = Pick<Grade, "temperature" | "tint" | "exposure">;

/** A grade's match controls: make a region of this clip neutral, pick the reference, or
 *  match a region of this clip to it. Each is measured where the playhead is. */
function matchControls(clip: string, effect: number, write: (light: Light) => void): HTMLElement {
  const ref = store.state.colorReference;
  const neutral = button("Neutral…", () => void matchColour(clip, { neutral: true }, "Something white or grey in life", write),
    "Drag over something in this clip that is white or grey in life: its white balance is set to make it so");
  const pick = button("Set reference…", () => void setReference(), "Drag over what the other shots should look like — a face, a wall — at the playhead. Kept until you set another");
  const match = button("Match…", () => ref && void matchColour(clip, ref, "The same thing in this clip", write),
    "Drag over the same thing in this clip: its white balance and exposure are set to look like the reference");
  match.disabled = !ref;
  // Held, not toggled: comparing is looking back and forth, and letting go is the way back.
  const before = button("Before", () => {}, "Hold to see this clip without its grade, at the playhead", "grade-before");
  let held = false;
  const hold = (on: boolean) => () => {
    if (on === held) return;
    const found = store.findClip(clip);
    if (on && !(found && covers(found.clip, store.state.playhead))) return toast("Put the playhead on this clip to compare it", "err");
    held = on;
    before.classList.toggle("on", on);
    showBefore(clip, effect, on);
  };
  before.addEventListener("pointerdown", hold(true));
  for (const end of ["pointerup", "pointerleave", "pointercancel"]) before.addEventListener(end, hold(false));
  const said = ref ? `Reference: ${ref.region.w}×${ref.region.h} px at ${fmtTime(ref.t, store.fps)} in ${binName(ref.sequence)}` : "No reference yet";
  const wrap = el("div", "match-controls");
  wrap.append(row("match-row", before, neutral, pick, match));
  // Several clips selected, and a reference: match them all at once, at its region.
  const n = store.selectedClips.length;
  if (ref && n > 1) wrap.append(row("match-row", button(`Match ${n} selected`, () => void matchSelectedToReference(),
    "Match every selected clip to the reference, each measured at the reference's region: for shots framed alike")));
  wrap.append(el("div", "match-note muted tiny", said));
  return wrap;
}

/** Pick the reference region at the playhead of the open sequence. */
async function setReference() {
  const region = await pickRegion("The reference: what the other shots should look like");
  if (!region) return;
  store.set({ colorReference: { sequence: store.state.sequence, t: store.state.playhead, region } });
  toast("Reference set: match other clips to it from their grade", "ok");
}

/** Measure `clip` at the playhead over a region the user picks, solve its grade's light to
 *  look like `reference`, and write it. A job that fails says so itself (the store toasts
 *  every job's end). */
async function matchColour(clip: string, reference: ColorReference, what: string, write: (light: Light) => void) {
  const found = store.findClip(clip);
  if (!found || !covers(found.clip, store.state.playhead)) {
    toast("Put the playhead on this clip: the match measures it there", "err");
    return;
  }
  const t = store.state.playhead;
  const region = await pickRegion(what);
  if (!region) return;
  try {
    const m = await measureMatch(clip, t, region, reference);
    if (!m) return;
    const light: Light = { temperature: m.grade.temperature ?? 0, tint: m.grade.tint ?? 0, exposure: m.grade.exposure ?? 0 };
    write(light);
    const said = `temperature ${light.temperature}, tint ${light.tint}, exposure ${light.exposure}`;
    toast(m.reached ? `Matched: ${said}` : `As near as the grade reaches: ${said}`, m.reached ? "ok" : "err");
  } catch (e) {
    toast((e as Error).message, "err");
  }
}

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

/** The control for one var; `key`, the document path it edits, keeps a colour picker open
 *  across the rebuild each edit causes. */
function varInput(spec: VarSpec, cur: unknown, onChange: (v: unknown) => void, key?: string): HTMLElement {
  switch (spec.type) {
    case "font":
      return fontInput(String(cur ?? ""), onChange);
    case "number":
      // A var that named its bounds gets the slider; one that did not is a bare number,
      // rather than a slider over a range nobody declared.
      return spec.min !== undefined && spec.max !== undefined
        ? sliderField(Number(cur), onChange, { min: spec.min, max: spec.max, step: spec.step ?? 0.01, soft: true })
        : numberField(Number(cur), onChange, { step: spec.step ?? 0.01, min: spec.min, max: spec.max });
    case "color":
      return colorField(String(cur ?? "#000000"), onChange, { key });
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
