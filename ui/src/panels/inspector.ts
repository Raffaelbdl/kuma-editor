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
import type { BinItem, Clip, Cue, Effect, PatchOp, Project, VarSpec } from "../types";
import { moveBinItem, placeBinItem } from "../edits";
import { binItem, binPath, canPlace, canTimeWarp, childrenOf, expandPreset, hasAudio, isColorSource, isFileSource, isFolderSource, isOverlaySource, isSequenceSource, isTimedMedia, placementsOf, seqDuration, wouldNest } from "../types";

const presetsPromise = api.effectPresets().catch(() => []);

/** Per effect kind: the value to add and how each field is edited (numeric with a step,
 *  a choice, free text, or JSON). Adding an effect kind is one row here plus the Rust
 *  model; a kind the daemon knows but this table does not renders with no fields. */
interface EffectSpec { add: Effect; num?: [string, number][]; enum?: [string, string[]][]; text?: string[]; json?: string[] }
const BLEND_MODES = ["normal", "multiply", "screen", "overlay", "darken", "lighten", "color_dodge", "color_burn", "hard_light", "soft_light", "difference", "exclusion"];
const EFFECTS: Partial<Record<Effect["type"], EffectSpec>> = {
  transform: { add: { type: "transform", x: 0, y: 0, scale: 1, rotation: 0, opacity: 1, keyframes: [] }, num: [["x", 1], ["y", 1], ["scale", 0.05], ["rotation", 1], ["opacity", 0.05]], json: ["keyframes"] },
  crop: { add: { type: "crop", left: 0, right: 0, top: 0, bottom: 0, radius: 0 }, num: [["left", 0.01], ["right", 0.01], ["top", 0.01], ["bottom", 0.01], ["radius", 0.01]] },
  blend: { add: { type: "blend", mode: "screen" }, enum: [["mode", BLEND_MODES]] },
  blur: { add: { type: "blur", radius: 4 }, num: [["radius", 0.5]] },
  color: { add: { type: "color", brightness: 0, contrast: 1, saturation: 1, gamma: 1 }, num: [["brightness", 0.05], ["contrast", 0.05], ["saturation", 0.05], ["gamma", 0.05]] },
  fade: { add: { type: "fade", in: 0.5, out: 0.5 }, num: [["in", 0.1], ["out", 0.1]] },
  volume: { add: { type: "volume", keyframes: [{ t: 0, gain: 1 }] }, json: ["keyframes"] },
  mlt: { add: { type: "mlt", service: "frei0r.vignette", properties: {} }, text: ["service"], json: ["properties"] },
};

export class Inspector {
  constructor(private host: HTMLElement) {
    store.select((s) => [s.project, s.selection, s.templates, s.sequence], () => this.render());
  }

  private render() {
    const sel = store.state.selection;
    this.host.innerHTML = "";
    if (sel?.kind === "bin") return this.renderBinEntry(sel.id);
    const f = store.findClip(sel?.kind === "clip" ? sel.id : null);
    if (!f) {
      this.host.innerHTML = `<h3>Inspector</h3>
        <p class="muted">Select a <b>bin entry</b> to change what it is, or a <b>clip</b> to change where and how it plays. Drag from the bin onto the timeline to place one.</p>
        <h3>Shortcuts</h3><p class="muted"><kbd>Space</kbd> play/pause · <kbd>←</kbd><kbd>→</kbd> frame step · <kbd>Del</kbd> delete · <kbd>Ctrl+Z</kbd>/<kbd>Ctrl+Shift+Z</kbd> undo/redo · <kbd>Ctrl+wheel</kbd> zoom · <kbd>S</kbd> split at playhead</p>`;
      return;
    }
    const { clip } = f;
    const path = store.clipPath(clip.id)!;
    const rep = (sub: string, value: unknown, note: string) => store.patch([{ op: "replace", path: `${path}${sub}`, value }], note);

    const h = el("h3", "", "Clip ");
    h.appendChild(el("span", "mono muted", clip.id));
    this.host.appendChild(h);
    this.host.appendChild(el("p", "muted tiny", "This placement only."));

    const item = binItem(store.state.project!, clip);
    const frame = 1 / store.fps;
    this.host.appendChild(field("Start", numInput(clip.start, (v) => rep("/start", v, "start"), frame)));
    this.host.appendChild(field("End", numInput(clip.end, (v) => rep("/end", v, "end"), frame)));
    // MLT cannot time-warp a timeline, so the field would only ever be refused.
    if (canTimeWarp(item)) {
      this.host.appendChild(field("Speed", numInput(clip.speed, (v) => rep("/speed", v, "speed"), 0.05)));
    }
    if (isTimedMedia(item)) {
      this.host.appendChild(field("Source in", numInput(clip.in ?? 0, (v) => rep("/in", v, "in"), frame)));
      this.host.appendChild(field("Volume", numInput(clip.volume, (v) => rep("/volume", v, "volume"), 0.05)));
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
    clip.effects.forEach((e, ei) => {
      const box = el("div", "effect");
      const head = el("div", "head");
      head.append(el("span", "", e.type), button("×", () => store.patch([{ op: "remove", path: `${path}/effects/${ei}` }], `remove ${e.type}`)));
      box.appendChild(head);
      const set = (key: string, v: unknown) => store.patch([{ op: "replace", path: `${path}/effects/${ei}`, value: { ...e, [key]: v } }], `${e.type}.${key}`);
      const rec = e as unknown as Record<string, unknown>;
      const spec = EFFECTS[e.type] ?? { add: e };
      const defaults = spec.add as unknown as Record<string, unknown>;
      const cur = (key: string) => rec[key] ?? defaults[key];
      for (const [key, step] of spec.num ?? []) box.appendChild(field(key, numInput(Number(cur(key)), (v) => set(key, v), step)));
      for (const [key, choices] of spec.enum ?? []) box.appendChild(field(key, selectInput(choices.map((value) => ({ value })), String(cur(key)), (v) => set(key, v))));
      for (const key of spec.text ?? []) box.appendChild(field(key, textInput(String(cur(key) ?? ""), (v) => set(key, v))));
      for (const key of spec.json ?? []) box.appendChild(field(key, jsonInput(cur(key) ?? null, (v) => set(key, v))));
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

function numInput(value: number, onChange: (v: number) => void, step = 1): HTMLInputElement {
  const i = el("input");
  i.type = "number";
  i.step = String(step);
  i.value = Number.isFinite(value) ? String(Number(value.toFixed(4))) : "";
  i.onchange = () => { const v = Number(i.value); if (Number.isFinite(v)) onChange(v); };
  return i;
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
    case "number": {
      const wrap = el("div", "row");
      const r = el("input");
      r.type = "range";
      r.min = String(spec.min ?? 0); r.max = String(spec.max ?? 1); r.step = String(spec.step ?? 0.01);
      r.value = String(cur);
      const n = numInput(Number(cur), onChange, spec.step ?? 0.01);
      n.style.width = "70px";
      r.oninput = () => { n.value = r.value; };
      r.onchange = () => onChange(Number(r.value));
      wrap.append(r, n);
      return wrap;
    }
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
      const p = (cur ?? { x: 0.5, y: 0.5 }) as { x: number; y: number };
      const wrap = el("div", "row");
      wrap.append(numInput(p.x, (v) => onChange({ x: v, y: p.y }), 0.01), numInput(p.y, (v) => onChange({ x: p.x, y: v }), 0.01));
      return wrap;
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
