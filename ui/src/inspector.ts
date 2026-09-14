// Properties panel: clip fields, template vars (from the manifest), effects.

import { api } from "./api";
import { button, el, options } from "./dom";
import { store, toast } from "./store";
import type { Clip, Effect, PatchOp, VarSpec } from "./types";
import { expandPreset, isAsset, isOverlay } from "./types";

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
    store.select((s) => [s.project, s.selection, s.templates], () => this.render());
  }

  private render() {
    const f = store.findClip(store.state.selection);
    this.host.innerHTML = "";
    if (!f) {
      this.host.innerHTML = `<h3>Inspector</h3><p class="muted">Select a clip. Drop templates or assets from the library onto the timeline.</p>
        <h3>Shortcuts</h3><p class="muted"><kbd>Space</kbd> play/pause · <kbd>←</kbd><kbd>→</kbd> frame step · <kbd>Del</kbd> delete · <kbd>Ctrl+Z</kbd>/<kbd>Ctrl+Shift+Z</kbd> undo/redo · <kbd>Ctrl+wheel</kbd> zoom · <kbd>S</kbd> split at playhead</p>`;
      return;
    }
    const { clip, ti, ci } = f;
    const path = `/tracks/${ti}/clips/${ci}`;
    const rep = (sub: string, value: unknown, note: string) => store.patch([{ op: "replace", path: `${path}${sub}`, value }], note);

    const h = el("h3", "", "Clip ");
    h.appendChild(el("span", "mono muted", clip.id));
    this.host.appendChild(h);

    const frame = 1 / store.fps;
    this.host.appendChild(field("Start", numInput(clip.start, (v) => rep("/start", v, "start"), frame)));
    this.host.appendChild(field("End", numInput(clip.end, (v) => rep("/end", v, "end"), frame)));
    this.host.appendChild(field("Speed", numInput(clip.speed, (v) => rep("/speed", v, "speed"), 0.05)));
    if (isAsset(clip)) {
      this.host.appendChild(field("Source in", numInput(clip.src.in ?? 0, (v) => rep("/src/in", v, "in"), frame)));
      this.host.appendChild(field("Volume", numInput(clip.volume, (v) => rep("/volume", v, "volume"), 0.05)));
      const m = el("input"); m.type = "checkbox"; m.checked = clip.muted;
      m.onchange = () => rep("/muted", m.checked, "mute");
      this.host.appendChild(field("Muted", m));
    }
    this.host.appendChild(field("Label", textInput(clip.label, (v) => rep("/label", v, "label"))));

    if (isOverlay(clip)) this.renderVars(clip, path);
    this.renderEffects(clip, path);

    const del = button("Delete clip", () => store.patch([{ op: "remove", path }], `delete ${clip.id}`));
    del.style.marginTop = "12px";
    this.host.appendChild(del);
  }

  private renderVars(clip: Clip & { src: { overlay: string; vars?: Record<string, unknown> } }, path: string) {
    const tpl = store.state.templates.find((t) => t.id === clip.src.overlay);
    this.host.appendChild(el("h3", "", `Template · ${tpl?.manifest.name ?? clip.src.overlay}`));
    if (!tpl) {
      this.host.appendChild(el("p", "muted", "Unknown template."));
      return;
    }
    const vars = clip.src.vars ?? {};
    const setVar = (k: string, v: unknown) => store.patch([{ op: "add", path: `${path}/src/vars`, value: { ...vars, [k]: v } }], `var ${k}`);
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
      for (const [key, choices] of spec.enum ?? []) {
        const sel = el("select");
        options(sel, choices.map((value) => ({ value })), String(cur(key)));
        sel.onchange = () => set(key, sel.value);
        box.appendChild(field(key, sel));
      }
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
  const t = el("textarea");
  t.value = JSON.stringify(value, null, 1);
  t.onchange = () => { try { onChange(JSON.parse(t.value)); t.style.borderColor = ""; } catch { t.style.borderColor = "#e5484d"; } };
  return t;
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
    case "boolean": {
      const b = el("input");
      b.type = "checkbox";
      b.checked = Boolean(cur);
      b.onchange = () => onChange(b.checked);
      return b;
    }
    case "enum": {
      const s = el("select");
      options(s, (spec.options ?? []).map((value) => ({ value })), String(cur));
      s.onchange = () => onChange(s.value);
      return s;
    }
    case "point": {
      const p = (cur ?? { x: 0.5, y: 0.5 }) as { x: number; y: number };
      const wrap = el("div", "row");
      wrap.append(numInput(p.x, (v) => onChange({ x: v, y: p.y }), 0.01), numInput(p.y, (v) => onChange({ x: p.x, y: v }), 0.01));
      return wrap;
    }
    case "text": {
      const t = el("textarea");
      t.value = String(cur ?? "");
      t.onchange = () => onChange(t.value);
      return t;
    }
    default:
      return textInput(String(cur ?? ""), onChange);
  }
}
