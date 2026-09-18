// Properties panel.
//
// It shows one of two things, and the split is the model's, not a layout choice: what is
// shown lives on the **bin entry** (its name, its file, an overlay's vars) and when it is
// shown lives on the **clip** (start, end, speed, trim, effects). So selecting an entry
// edits the thing itself — and every placement of it follows — while selecting a clip
// edits only that placement. A clip has no name for the same reason; it has a label.

import { api } from "../api";
import { Cmd, run } from "../commands";
import { button, checkbox, el, options, plural, row, selectInput } from "../dom";
import { store, toast } from "../store";
import type { BinItem, Clip, Cue, Ease, Effect, PatchOp, Project, VarSpec } from "../types";
import { moveBinItem, placeBinItem } from "../edits";
import { Animation } from "../keyframes";
import { icon } from "../icons";
import type { NumberOpts } from "../props";
import { keyStrip, numberField, sliderField, vectorField } from "../props";
import { binItem, binPath, canPlace, canTimeWarp, childrenOf, expandPreset, hasAudio, isColorSource, isFileSource, isFolderSource, isOverlaySource, isSequenceSource, isTimedMedia, placementsOf, seqDuration, wouldNest } from "../types";

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
  | { kind: "json"; key: string; label?: string }
  /** This effect's keyframes: the row of buttons and the strip, and the fact that the
   *  fields above it are then values *at the playhead*. `fields` is what a key carries. */
  | { kind: "keys"; key: string; fields: string[]; min?: number };

/** Per effect kind: the value `+ add effect` inserts, and the properties it shows, in the
 *  order they are shown. Adding an effect kind is one row here plus the Rust model; a kind
 *  the daemon knows but this table does not renders with no fields.
 *
 *  The units are the model's, and the tooltips say so where it is not obvious: transform
 *  offsets are pixels from the centred position, a crop is a fraction of each edge, a
 *  blur radius is pixels. */
interface EffectSpec { add: Effect; props?: PropSpec[] }
const EASES: Ease[] = ["linear", "hold", "smooth"];
const BLEND_MODES = ["normal", "multiply", "screen", "overlay", "darken", "lighten", "color_dodge", "color_burn", "hard_light", "soft_light", "difference", "exclusion"];
const EFFECTS: Partial<Record<Effect["type"], EffectSpec>> = {
  transform: {
    add: { type: "transform", x: 0, y: 0, scale: 1, rotation: 0, opacity: 1, keyframes: [] },
    props: [
      { kind: "vec", label: "Position", parts: [
        { key: "x", tag: "X", axis: "x", step: 1, suffix: "px", title: "Pixels right of the centred position" },
        { key: "y", tag: "Y", axis: "y", step: 1, suffix: "px", title: "Pixels down from the centred position" },
      ] },
      // The model's own bound: a scale of 0 is refused, so the field stops at one step.
      { kind: "num", key: "scale", step: 0.01, min: 0.01, suffix: "×", title: "Multiplies the composition size" },
      { kind: "num", key: "rotation", step: 1, suffix: "°" },
      { kind: "slider", key: "opacity", min: 0, max: 1, step: 0.01 },
      { kind: "keys", key: "keyframes", fields: ["x", "y", "scale", "rotation", "opacity"] },
    ],
  },
  crop: {
    add: { type: "crop", left: 0, right: 0, top: 0, bottom: 0, radius: 0 },
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
    add: { type: "color", brightness: 0, contrast: 1, saturation: 1, gamma: 1 },
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
      { kind: "keys", key: "keyframes", fields: ["gain"], min: 1 },
    ],
  },
  mlt: { add: { type: "mlt", service: "frei0r.vignette", properties: {} }, props: [{ kind: "text", key: "service" }, { kind: "json", key: "properties" }] },
};

/** Whether the playhead is inside the selected clip. */
function playheadInClip(): boolean {
  const f = store.findClip(store.selectedClip);
  return !!f && store.state.playhead >= f.clip.start && store.state.playhead < f.clip.end;
}

/** A key as a label: `brightness` → `Brightness`, `in` → `In`. */
const title = (key: string) => key.charAt(0).toUpperCase() + key.slice(1);

/**
 * How a row reads and writes its effect. Without keyframes that is the effect's own
 * fields; with them it is the value *at the playhead*, and writing goes into the key
 * there — which is the whole of the keyframe UI as far as a field is concerned.
 */
interface PropCtx {
  read: (key: string) => unknown;
  write: (key: string, v: unknown) => void;
  /** This field is keyed, so it is a value in time rather than a value. */
  animated: (key: string) => boolean;
  /** …and the playhead is not in the clip, so there is no "now" to write to. */
  frozen: boolean;
  /** A setter for a keyed field to follow the playhead with, or undefined. */
  live: (key: string) => NumberOpts["live"];
}

/** One property row, whatever shape it is. */
function propRow(p: PropSpec, ctx: PropCtx): HTMLElement {
  const row = build(p, ctx);
  // What this row edits, on the row: the panel styles by it, and a test can name a
  // property rather than counting divs.
  row.dataset.prop = p.kind === "vec" ? p.label.toLowerCase() : p.key;
  return row;
}

function build(p: PropSpec, ctx: PropCtx): HTMLDivElement {
  const { read, write } = ctx;
  const num = (key: string) => Number(read(key) ?? 0);
  /** What a numeric field needs on top of its own spec when the value is keyed. */
  const timed = (key: string) => ({ readOnly: ctx.animated(key) && ctx.frozen, live: ctx.live(key) });
  switch (p.kind) {
    case "vec":
      return field(p.label, vectorField(p.parts.map((part) => ({ ...part, ...timed(part.key), value: num(part.key), commit: (v: number) => write(part.key, v) }))));
    case "num":
      return field(p.label ?? title(p.key), numberField(num(p.key), (v) => write(p.key, v), { ...p, ...timed(p.key) }));
    case "slider":
      return field(p.label ?? title(p.key), sliderField(num(p.key), (v) => write(p.key, v), { ...p, ...timed(p.key) }));
    case "enum":
      return field(p.label ?? title(p.key), selectInput(p.options.map((value) => ({ value })), String(read(p.key)), (v) => write(p.key, v)));
    case "text":
      return field(p.label ?? title(p.key), textInput(String(read(p.key) ?? ""), (v) => write(p.key, v)));
    case "json":
      return field(p.label ?? title(p.key), jsonInput(read(p.key) ?? null, (v) => write(p.key, v)));
    case "keys":
      // Built by `keyRow`, which needs the Animation rather than one field's value.
      return el("div");
  }
}

/**
 * The keyframe row: what there is to do to an effect's keys, and where they are. It is
 * one row of buttons and a strip — the panel deliberately does not grow a track editor,
 * and the clip in the timeline carries the same keys so you can see them while scrubbing.
 */
function keyRow(anim: Animation, follow: (fn: () => void) => void): HTMLElement {
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
  tools.append(el("span", "muted tiny", "keys"), prev, add, del, next, el("span", "spacer"));
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
  ease.title = "How this key reaches the next one: a straight line, a hold, or a curve through its neighbours";
  wrap.appendChild(field("Ease", ease));

  const hint = el("div", "muted tiny kf-hint");
  const refresh = () => {
    add.disabled = !anim.inside || anim.index >= 0;
    del.disabled = !anim.inside || anim.index < 0 || anim.keys.length <= anim.min;
    ease.disabled = anim.index < 0;
    ease.value = anim.ease;
    hint.textContent = anim.inside ? "" : "The playhead is outside this clip — click the strip, or use ‹ ›, to edit a key.";
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
        <p class="muted">Select a <b>bin entry</b> to change what it is, or a <b>clip</b> to change where and how it plays. Drag from the bin onto the timeline to place one.</p>
        <h3>Shortcuts</h3><p class="muted"><kbd>Space</kbd> play/pause · <kbd>←</kbd><kbd>→</kbd> frame step (<kbd>Shift</kbd> a second, <kbd>Alt</kbd> to the next cut) · <kbd>Home</kbd>/<kbd>End</kbd> ends · <kbd>Del</kbd> delete · <kbd>Ctrl+Z</kbd>/<kbd>Ctrl+Shift+Z</kbd> undo/redo · <kbd>Ctrl+wheel</kbd> zoom · <kbd>S</kbd> split at playhead</p>
        <p class="muted"><kbd>Ctrl+C</kbd>/<kbd>Ctrl+X</kbd>/<kbd>Ctrl+V</kbd> copy, cut and paste clips — a paste lands at the playhead, on the track you last clicked. <kbd>Ctrl+click</kbd> a clip to select more than one; those keep their own tracks and spacing.</p>`;
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
      const box = el("div", "effect");
      const head = el("div", "head");
      head.append(el("span", "", title(e.type)), button("×", () => store.patch([{ op: "remove", path: `${path}/effects/${ei}` }], `remove ${e.type}`), `Remove this ${e.type}`));
      box.appendChild(head);
      const rec = e as unknown as Record<string, unknown>;
      const spec = EFFECTS[e.type] ?? { add: e };
      const defaults = spec.add as unknown as Record<string, unknown>;
      const cur = (key: string) => rec[key] ?? defaults[key];
      const replace = (value: unknown, note: string) => void store.patch([{ op: "replace", path: `${path}/effects/${ei}`, value }], `${e.type} ${note}`);
      const set = (key: string, v: unknown) => replace({ ...e, [key]: v }, `${key}`);

      // Keyframes, when this kind has them: the fields above the strip then read and write
      // the value *at the playhead* rather than a static one.
      const keySpec = (spec.props ?? []).find((p) => p.kind === "keys") as Extract<PropSpec, { kind: "keys" }> | undefined;
      const anim = keySpec && new Animation({
        effect: rec,
        keyField: keySpec.key,
        fields: keySpec.fields,
        min: keySpec.min ?? 0,
        base: (f) => Number(cur(f) ?? 0),
        clip,
        commit: (next, note) => replace(next, note),
      });
      const timed = (key: string) => !!anim?.animated && anim.fields.includes(key);
      const ctx: PropCtx = {
        read: (key) => (timed(key) ? anim!.at(key) : cur(key)),
        write: (key, v) => (timed(key) ? anim!.setAt(key, Number(v)) : set(key, v)),
        animated: timed,
        frozen: !!anim && !anim.inside,
        // A keyed field follows the playhead: one subscription per effect updates the
        // values in place, because re-rendering this panel 25 times a second is what
        // `store.select` exists to prevent.
        live: (key) => (timed(key) ? (set) => followers.push(() => set(anim!.at(key))) : undefined),
      };
      for (const p of spec.props ?? []) box.appendChild(p.kind === "keys" ? keyRow(anim!, (fn) => followers.push(fn)) : propRow(p, ctx));
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

/** Every plain number in the panel, so a cue's start and a preset's parameter are dragged
 *  the way an effect's fields are. */
function numInput(value: number, onChange: (v: number) => void, step = 1): HTMLElement {
  return numberField(value, onChange, { step });
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
      numInput(c.start, (v) => change(i, { start: v }), 0.01),
      numInput(c.end, (v) => change(i, { end: v }), 0.01),
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
