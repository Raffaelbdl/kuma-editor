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
import { store, toast } from "../store";
import type { BinItem, Clip, Cue, Ease, Effect, PatchOp, Project, VarSpec } from "../types";
import { moveBinItem, placeBinItem } from "../edits";
import type { Animation } from "../keyframes";
import { animationOf } from "../keyframes";
import { icon, svgEl } from "../icons";
import type { NumberOpts } from "../props";
import { keyStrip, numberField, showNumber, sliderField, vectorField } from "../props";
import { ANIMATABLE, binItem, binPath, canPlace, canTimeWarp, childrenOf, covers, expandPreset, hasAudio, isColorSource, isFileSource, isFolderSource, isOverlaySource, isSequenceSource, isTimedMedia, placementsOf, seqDuration, wouldNest } from "../types";

const presetsPromise = api.effectPresets().catch(() => []);

/** What one field of an effect looks like. The shapes come from `props.ts` — a number you
 *  drag, a vector wearing its axis colours, a bounded amount with a slider — and this says
 *  which shape each field is. Nothing else in the panel needs to know. */
type PropSpec =
  | ({ kind: "num"; key: string; label?: string } & NumberOpts)
  | ({ kind: "slider"; key: string; label?: string; min: number; max: number; /** The range is a suggestion, not a limit: the field may be typed past it. */ soft?: boolean } & NumberOpts)
  | { kind: "vec"; label: string; parts: ({ key: string } & NumberOpts)[] }
  | { kind: "enum"; key: string; label?: string; options: string[] }
  | { kind: "text"; key: string; label?: string }
  | { kind: "json"; key: string; label?: string };

/** Per effect kind: the value `+ add effect` inserts, and the properties it shows, in the
 *  order they are shown. Adding an effect kind is one row here plus the Rust model; a kind
 *  the daemon knows but this table does not renders with no fields. A kind in `ANIMATABLE`
 *  gets the keyframe row after its fields, and those fields then read and write the value
 *  *at the playhead*.
 *
 *  The units are the model's, and the tooltips say so where it is not obvious: transform
 *  offsets are pixels from the centred position, a crop is a fraction of each edge, a
 *  blur radius is pixels. */
interface EffectSpec { add: Effect; props?: PropSpec[] }
const EASES: Ease[] = ["linear", "hold", "smooth"];
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
      { kind: "slider", key: "radius", min: 0, max: 1, step: 0.01, title: "Corner radius, as a fraction of the shorter side" },
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
  fade: {
    add: { type: "fade", in: 0.5, out: 0.5 },
    props: [{ kind: "vec", label: "Duration", parts: [
      { key: "in", tag: "In", step: 0.1, min: 0, suffix: "s" },
      { key: "out", tag: "Out", step: 0.1, min: 0, suffix: "s" },
    ] }],
  },
  // A volume effect is its keyframes — the model refuses one with none, so `min` is 1 and
  // there is no un-animating it; removing the effect is how you stop.
  volume: {
    add: { type: "volume", keyframes: [{ t: 0, gain: 1 }] },
    props: [
      { kind: "num", key: "gain", step: 0.05, min: 0, title: "Gain at the playhead: 1 is the clip as recorded" },
    ],
  },
  mlt: { add: { type: "mlt", service: "frei0r.vignette", properties: {} }, props: [{ kind: "text", key: "service" }, { kind: "json", key: "properties" }] },
};

/** Which effects have their curve view open, and on which parameter — by `clip/effect`,
 *  so the view survives the re-render every edit causes. View state; nothing saves it. */
const openCurves = new Map<string, string>();

/** Which effects are folded shut, by the same `clip/effect`. Closed is what is *stored*,
 *  so a new effect and a newly selected clip open: the panel says what it has rather than
 *  making you open three boxes to find out. View state, like the curves — a fold is about
 *  looking, not about the video, and nothing saves it. */
const closedEffects = new Set<string>();

/** How many points the curve is drawn with. Enough for a Catmull-Rom to look like one. */
const CURVE_POINTS = 64;

/**
 * The curve view: one parameter's value over the clip, the keys on it as points you can
 * drag in both axes. The graph is what the strip cannot say — the *shape* between keys,
 * which is the whole question with a hold or a smooth — and a point dragged up is a
 * value edited without a number being typed. Read-only in one respect: it draws the
 * curve the compiler will produce (`paramAt` is the same sampler the preview uses), it
 * does not let you draw one.
 */
function curveView(anim: Animation, param: string, onParam: (p: string) => void, follow: (fn: () => void) => void): HTMLElement {
  const wrap = el("div", "kf-curve");
  if (anim.fields.length > 1) {
    const pick = selectInput(anim.fields.map((value) => ({ value, label: title(value) })), param, onParam);
    pick.className = "kf-curve-param";
    wrap.appendChild(pick);
  }
  const plot = el("div", "kf-plot");
  const dur = anim.duration;

  // Sampled on a grid, and at every key — and a hair before every key, so a hold draws as
  // the step it is rather than a slant one grid-cell wide, and every key lies on the
  // line whatever its time.
  const times = new Set<number>(Array.from({ length: CURVE_POINTS + 1 }, (_, i) => (i / CURVE_POINTS) * dur));
  for (const k of anim.keys) { times.add(Math.max(0, k.t - 1e-6)); times.add(k.t); }
  const grid = [...times].sort((a, b) => a - b).map((t) => ({ t, v: anim.at(param, t) }));
  const samples = grid.map((g) => g.v);
  // The value axis fits what is there — the curve, which passes through every key — with
  // a little room, and a flat line gets a unit of room either side rather than a division
  // by nothing.
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

/** A key as a label: `brightness` → `Brightness`, `in` → `In`. */
const title = capitalize;

/**
 * How a row reads and writes its effect. Without keyframes that is the effect's own
 * fields; with them it is the value *at the playhead*, and writing goes into the key
 * there — which is the whole of the keyframe UI as far as a field is concerned.
 */
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
      input = textInput(String(read(p.key) ?? ""), (v) => write(p.key, v));
      break;
    case "json":
      input = jsonInput(read(p.key) ?? null, (v) => write(p.key, v));
      break;
  }
  const row = field(label, input);
  row.dataset.prop = p.kind === "vec" ? p.label.toLowerCase() : p.key;
  return row;
}

/**
 * The keyframe row: what there is to do to an effect's keys, and where they are. It is
 * one row of buttons and a strip — the panel deliberately does not grow a track editor,
 * and the clip in the timeline carries the same keys so you can see them while scrubbing.
 */
function keyRow(anim: Animation, id: string, follow: (fn: () => void) => void): HTMLElement {
  const wrap = el("div");
  wrap.dataset.prop = "keys";
  if (!anim.animated) {
    wrap.appendChild(field("Keys", button("◆ Animate", () => anim.animate(), "Keyframe this effect: adds a key at the playhead holding the values it has now")));
    return wrap;
  }

  // The toolbar takes the whole width rather than sitting in the value column: it is about
  // the effect's timing, not about one of its values, and four buttons do not fit there.
  const tools = el("div", "kf-tools");
  const prev = button(icon("chevron-left", 12), () => anim.step(-1), "Previous key", "icon");
  const next = button(icon("chevron-right", 12), () => anim.step(1), "Next key", "icon");
  const add = button("+ key", () => anim.addKey(), "Add a key at the playhead, holding the values it has there");
  const del = button("− key", () => anim.removeKey(), "Remove the key at the playhead");
  // The curve view is off until asked for, and stays where it was put across the
  // re-render every edit causes: the panel must not grow a graph per effect unbidden.
  const curveHost = el("div", "kf-curve-host");
  const curve = button("curve", () => {
    if (openCurves.has(id)) openCurves.delete(id);
    else openCurves.set(id, anim.fields[0]);
    showCurve();
  }, "Show this effect's values over the clip as a graph, with the keys to drag");
  // One follower for whichever curve is showing: the view is rebuilt on every toggle and
  // parameter pick, and each build must not leave its predecessor's playhead marker
  // following along behind a detached graph.
  let curveFollower: (() => void) | null = null;
  follow(() => curveFollower?.());
  const showCurve = () => {
    const param = openCurves.get(id);
    curve.classList.toggle("on", !!param);
    curveFollower = null;
    curveHost.replaceChildren(...(param ? [curveView(anim, param, (p) => { openCurves.set(id, p); showCurve(); }, (fn) => { curveFollower = fn; })] : []));
  };
  // No caption: the strip under it says what these are, and the width is spoken for.
  tools.append(prev, add, del, next, curve, el("span", "spacer"));
  // A volume effect must keep a key, so it has no "stop animating" — removing the effect is
  // how that is said.
  if (anim.min === 0) tools.append(button("Clear", () => anim.clear(), "Stop keyframing, keeping the values at the playhead"));
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

  // How the key under the playhead reaches the next one. It is a property of *that* key,
  // so it sits under the strip rather than among the effect's values, and means nothing
  // when the playhead is not on one.
  const ease = selectInput(EASES.map((value) => ({ value })), anim.ease, (v) => anim.setEase(v as Ease));
  ease.className = "kf-ease";
  ease.title = "How the key under the playhead reaches the next one: a straight line, a hold, or a curve through its neighbours. It takes effect as you pick it.";
  // "Ease → next", not "Ease": it is a property of the *segment* this key opens, which
  // is the one thing about easing that has to be said out loud. Saying only "Ease" reads
  // as "this key's easing", and then setting it on the last key looks like a bug.
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
    del.disabled = !anim.inside || i < 0 || anim.keys.length <= anim.min;
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
    store.select((s) => [s.project, s.selection, s.templates, s.sequence, playheadInClip()], () => this.render());
  }

  private render() {
    for (const stop of this.off) stop();
    this.off = [];
    const sel = store.state.selection;
    this.host.innerHTML = "";
    if (sel?.kind === "bin") return this.renderBinEntry(sel.id);
    const f = store.findClip(store.selectedClip);
    if (!f) {
      this.host.innerHTML = `<h3>Inspector</h3>
        <p class="muted">Select a <b>bin entry</b> to change what it is, or a <b>clip</b> to change where and how it plays. Drag from the bin onto the timeline to place one.</p>`;
      // Not a list of keys copied out here: there is one, it is the bindings themselves,
      // and two copies of it would disagree within a week.
      this.host.appendChild(button("Keyboard shortcuts", () => run(Cmd.shortcuts), `Every key and gesture the editor has (${keysFor(Cmd.shortcuts)})`));
      return;
    }
    const { clip } = f;
    const path = store.clipPath(clip.id)!;
    const rep = (sub: string, value: unknown, note: string) => store.patch([{ op: "replace", path: `${path}${sub}`, value }], note);

    const h = el("h3", "", "Clip ");
    h.appendChild(el("span", "mono muted", clip.id));
    this.host.appendChild(h);
    // With several selected the panel still edits one — the last one clicked. Saying so is
    // the honest version: the alternative is fields that look like they apply to all of
    // them. Copy, cut, delete and split do apply to all of them, and say so in the strip.
    const n = store.selectedClips.length;
    this.host.appendChild(el("p", "muted tiny", n > 1 ? `This placement only — ${n} clips are selected, and this is the last one clicked.` : "This placement only."));

    const item = binItem(store.state.project!, clip);
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
      this.host.appendChild(field("Volume", sliderField(clip.volume, (v) => void rep("/volume", v, "volume"), { min: 0, max: 2, step: 0.01, soft: true, title: "Gain: 1 is the clip as recorded" })));
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
    this.renderEffects(clip, path);

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
    // The same gesture for the other kind of entry that has an editor of its own.
    if (isOverlaySource(item.source)) {
      const open = button("Open", () => store.openOverlay(id), "Iterate on it in the overlay workbench");
      (open as HTMLButtonElement).disabled = id === store.state.tab;
      actions.appendChild(open);
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
  }

  /** The `source` half of an entry — the part that differs per kind. */
  private renderSource(p: Project, id: string, item: BinItem, bp: string) {
    if (isFileSource(item.source)) {
      this.host.appendChild(field("Path", textInput(item.source.path, (v) => store.patch([{ op: "replace", path: `${bp}/source/path`, value: v }], `path ${id}`))));
      const m = item.meta;
      const facts = [m?.duration ? `${m.duration.toFixed(2)}s` : "", m?.width ? `${m.width}×${m.height}` : "", m?.has_audio ? "audio" : ""].filter(Boolean).join(" · ");
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
    const tpl = store.state.templates.find((t) => t.id === source.template);
    this.host.appendChild(field("Template", el("div", "mono muted", source.template)));
    if (!tpl) {
      this.host.appendChild(el("p", "muted", "Unknown template — is its library on the search path?"));
      return;
    }
    // The row cannot show this and stay one line, but an overlay is the one entry kind
    // whose look is the point, so it moves here rather than being dropped. Absent until
    // the `thumbs` job has run, and it removes itself when there is nothing to show.
    const thumb = el("img", "insp-thumb");
    // The entry's own moving preview when the `thumbs` job has made one, its still
    // otherwise, and nothing at all until then.
    thumb.src = `/api/bin/${encodeURIComponent(id)}/preview.webp`;
    thumb.alt = "";
    thumb.onerror = () => {
      if (thumb.src.endsWith("preview.webp")) thumb.src = `/api/bin/${encodeURIComponent(id)}/thumb.png`;
      else thumb.remove();
    };
    this.host.appendChild(thumb);

    const vars = source.vars ?? {};
    const setVar = (k: string, v: unknown) => store.patch([{ op: "add", path: `${bp}/source/vars`, value: { ...vars, [k]: v } }], `var ${k}`);
    for (const [k, spec] of Object.entries(tpl.manifest.vars)) {
      const cur = k in vars ? vars[k] : spec.default;
      this.host.appendChild(field(spec.label ?? k, varInput(spec, cur, (v) => setVar(k, v))));
    }
  }

  private renderEffects(clip: Clip, path: string) {
    this.host.appendChild(el("h3", "", "Effects"));
    // Everything that follows the playhead without the panel being rebuilt for it.
    const followers: (() => void)[] = [];
    clip.effects.forEach((e, ei) => {
      const id = `${clip.id}/${ei}`;
      const box = el("div", `effect${closedEffects.has(id) ? " closed" : ""}`);
      // The fields live in a box of their own so that folding is one class on the effect,
      // and so the head — the one thing a closed effect still shows — is never part of it.
      const body = el("div", "effect-body");
      /** This effect's followers. They are the outer list's, but held here first: a closed
       *  effect follows nothing, and this is what makes that one check rather than one per
       *  field. */
      const mine: (() => void)[] = [];

      const head = el("div", "head");
      const disclose = button(icon("chevron-right", 11), () => fold(), `Show or hide this ${e.type}'s settings`, "disclose");
      disclose.append(title(e.type));
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
      head.append(disclose, button("×", () => store.patch([{ op: "remove", path: `${path}/effects/${ei}` }], `remove ${e.type}`), `Remove this ${e.type}`));
      box.append(head, body);
      const rec = e as unknown as Record<string, unknown>;
      const spec = EFFECTS[e.type] ?? { add: e };
      const defaults = spec.add as unknown as Record<string, unknown>;
      const cur = (key: string) => rec[key] ?? defaults[key];
      const replace = (value: unknown, note: string) => void store.patch([{ op: "replace", path: `${path}/effects/${ei}`, value }], `${e.type} ${note}`);
      const set = (key: string, v: unknown) => replace({ ...e, [key]: v }, `${key}`);

      // Keyframes, when this kind takes them: an animatable field reads through the one
      // sampler (which is the static value until there are keys), and once there are keys
      // it writes the value *at the playhead* rather than a static one.
      const anim = ANIMATABLE[e.type] ? animationOf(clip.id, ei) : null;
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
      // A keyframed effect says so in its head, because folding it away hides the strip
      // that would otherwise be how you know.
      if (anim?.animated) disclose.append(el("span", "keyed", "◆"));
      for (const p of spec.props ?? []) body.appendChild(propRow(p, ctx));
      if (anim) body.appendChild(keyRow(anim, id, (fn) => mine.push(fn)));
      if (mine.length) followers.push(() => { if (!closedEffects.has(id)) for (const f of mine) f(); });
      this.host.appendChild(box);
    });
    const add = el("select");
    options(add, Object.keys(EFFECTS).map((value) => ({ value })), undefined, "+ add effect…");
    add.onchange = () => {
      const kind = add.value as Effect["type"] | "";
      add.value = "";
      if (kind) store.patch([{ op: "add", path: `${path}/effects/-`, value: EFFECTS[kind]!.add }], `add ${kind}`);
    };
    this.host.appendChild(add);
    this.host.appendChild(this.presetPicker(path));
    if (followers.length) this.off.push(store.select((s) => [s.playhead], () => { for (const f of followers) f(); }));
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
  t.onchange = () => { try { onChange(JSON.parse(t.value)); t.style.borderColor = ""; } catch { t.style.borderColor = "#e5484d"; } };
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

function varInput(spec: VarSpec, cur: unknown, onChange: (v: unknown) => void): HTMLElement {
  switch (spec.type) {
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
