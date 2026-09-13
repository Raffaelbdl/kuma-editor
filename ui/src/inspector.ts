// Properties panel: clip fields, template vars (from the manifest), effects.

import { store } from "./store";
import type { Clip, Effect, PatchOp, VarSpec } from "./types";
import { isAsset, isOverlay } from "./types";

const EFFECT_DEFAULTS: Record<string, Effect> = {
  transform: { type: "transform", x: 0, y: 0, scale: 1, rotation: 0, opacity: 1 },
  crop: { type: "crop", left: 0, right: 0, top: 0, bottom: 0, radius: 0 },
  blend: { type: "blend", mode: "screen" },
  blur: { type: "blur", radius: 4 },
  color: { type: "color", brightness: 0, contrast: 1, saturation: 1, gamma: 1 },
  fade: { type: "fade", in: 0.5, out: 0.5 },
  mlt: { type: "mlt", service: "frei0r.vignette", properties: {} },
};
const BLEND_MODES = ["normal", "multiply", "screen", "overlay", "darken", "lighten", "color_dodge", "color_burn", "hard_light", "soft_light", "difference", "exclusion"];

export class Inspector {
  constructor(private host: HTMLElement) {
    store.subscribe(() => this.render());
  }

  private render() {
    const f = store.findClip(store.state.selection);
    this.host.innerHTML = "";
    if (!f) {
      this.host.innerHTML = `<h3>Inspector</h3><p class="muted">Select a clip. Drop templates or assets from the library onto the timeline.</p>
        <h3>Shortcuts</h3><p class="muted"><kbd>Space</kbd> play/pause · <kbd>←</kbd><kbd>→</kbd> frame step · <kbd>Del</kbd> delete · <kbd>Ctrl+Z</kbd>/<kbd>Ctrl+Y</kbd> undo/redo · <kbd>Ctrl+wheel</kbd> zoom · <kbd>S</kbd> split at playhead</p>`;
      return;
    }
    const { clip, ti, ci } = f;
    const path = `/tracks/${ti}/clips/${ci}`;
    const rep = (sub: string, value: unknown, note: string) => store.patch([{ op: "replace", path: `${path}${sub}`, value }], note);

    const h = document.createElement("div");
    h.innerHTML = `<h3>Clip <span class="mono muted">${clip.id}</span></h3>`;
    this.host.appendChild(h);

    this.host.appendChild(field("Start", numInput(clip.start, (v) => rep("/start", v, "start"), 1 / store.fps)));
    this.host.appendChild(field("End", numInput(clip.end, (v) => rep("/end", v, "end"), 1 / store.fps)));
    this.host.appendChild(field("Speed", numInput(clip.speed, (v) => rep("/speed", v, "speed"), 0.05)));
    if (isAsset(clip)) {
      this.host.appendChild(field("Source in", numInput(clip.src.in ?? 0, (v) => rep("/src/in", v, "in"), 1 / store.fps)));
      this.host.appendChild(field("Volume", numInput(clip.volume, (v) => rep("/volume", v, "volume"), 0.05)));
      const m = document.createElement("input"); m.type = "checkbox"; m.checked = clip.muted;
      m.onchange = () => rep("/muted", m.checked, "mute");
      this.host.appendChild(field("Muted", m));
    }
    this.host.appendChild(field("Label", textInput(clip.label ?? "", (v) => rep("/label", v, "label"))));

    if (isOverlay(clip)) this.renderVars(clip, path);
    this.renderEffects(clip, path);

    const del = document.createElement("button");
    del.textContent = "Delete clip";
    del.style.marginTop = "12px";
    del.onclick = () => store.patch([{ op: "remove", path }], `delete ${clip.id}`);
    this.host.appendChild(del);
  }

  private renderVars(clip: Clip & { src: { overlay: string; vars?: Record<string, unknown> } }, path: string) {
    const tpl = store.state.templates.find((t) => t.id === clip.src.overlay);
    const h = document.createElement("h3");
    h.textContent = `Template · ${tpl?.manifest.name ?? clip.src.overlay}`;
    this.host.appendChild(h);
    if (!tpl) {
      this.host.innerHTML += `<p class="muted">Unknown template.</p>`;
      return;
    }
    const vars = clip.src.vars ?? {};
    const setVar = (k: string, v: unknown) => {
      const next = { ...vars, [k]: v };
      return store.patch([{ op: clip.src.vars ? "replace" : "add", path: `${path}/src/vars`, value: next }], `var ${k}`);
    };
    for (const [k, spec] of Object.entries(tpl.manifest.vars)) {
      const cur = k in vars ? vars[k] : spec.default;
      this.host.appendChild(field(spec.label ?? k, varInput(spec, cur, (v) => setVar(k, v))));
    }
  }

  private renderEffects(clip: Clip, path: string) {
    const h = document.createElement("h3");
    h.textContent = "Effects";
    this.host.appendChild(h);
    clip.effects.forEach((e, ei) => {
      const box = document.createElement("div");
      box.className = "effect";
      const head = document.createElement("div");
      head.className = "head";
      head.innerHTML = `<span>${e.type}</span>`;
      const rm = document.createElement("button");
      rm.textContent = "×";
      rm.onclick = () => store.patch([{ op: "remove", path: `${path}/effects/${ei}` }], `remove ${e.type}`);
      head.appendChild(rm);
      box.appendChild(head);
      const set = (key: string, v: unknown) => {
        const next = { ...e, [key]: v } as Effect;
        return store.patch([{ op: "replace", path: `${path}/effects/${ei}`, value: next }], `${e.type}.${key}`);
      };
      const num = (key: string, val: number, step = 1) => box.appendChild(field(key, numInput(val, (v) => set(key, v), step)));
      switch (e.type) {
        case "transform":
          num("x", e.x ?? 0); num("y", e.y ?? 0); num("scale", e.scale ?? 1, 0.05); num("rotation", e.rotation ?? 0); num("opacity", e.opacity ?? 1, 0.05);
          box.appendChild(field("keyframes", jsonInput(e.keyframes ?? [], (v) => set("keyframes", v))));
          break;
        case "crop":
          num("left", e.left ?? 0, 0.01); num("right", e.right ?? 0, 0.01); num("top", e.top ?? 0, 0.01); num("bottom", e.bottom ?? 0, 0.01); num("radius", e.radius ?? 0, 0.01);
          break;
        case "blend": {
          const sel = document.createElement("select");
          for (const m of BLEND_MODES) { const o = document.createElement("option"); o.value = m; o.textContent = m; o.selected = m === e.mode; sel.appendChild(o); }
          sel.onchange = () => set("mode", sel.value);
          box.appendChild(field("mode", sel));
          break;
        }
        case "blur": num("radius", e.radius, 0.5); break;
        case "color": num("brightness", e.brightness ?? 0, 0.05); num("contrast", e.contrast ?? 1, 0.05); num("saturation", e.saturation ?? 1, 0.05); num("gamma", e.gamma ?? 1, 0.05); break;
        case "fade": num("in", e.in ?? 0, 0.1); num("out", e.out ?? 0, 0.1); break;
        case "volume": box.appendChild(field("keyframes", jsonInput(e.keyframes, (v) => set("keyframes", v)))); break;
        case "mlt":
          box.appendChild(field("service", textInput(e.service, (v) => set("service", v))));
          box.appendChild(field("properties", jsonInput(e.properties ?? {}, (v) => set("properties", v))));
          break;
      }
      this.host.appendChild(box);
    });
    const add = document.createElement("select");
    add.innerHTML = `<option value="">+ add effect…</option>` + Object.keys(EFFECT_DEFAULTS).map((k) => `<option value="${k}">${k}</option>`).join("");
    add.onchange = () => {
      if (!add.value) return;
      const ops: PatchOp[] = [{ op: "add", path: `${path}/effects/-`, value: EFFECT_DEFAULTS[add.value] }];
      store.patch(ops, `add ${add.value}`);
    };
    this.host.appendChild(add);
  }
}

// ---- inputs --------------------------------------------------------------------

function field(label: string, input: HTMLElement): HTMLDivElement {
  const el = document.createElement("div");
  el.className = "field";
  const l = document.createElement("label");
  l.textContent = label;
  el.append(l, input);
  return el;
}

function numInput(value: number, onChange: (v: number) => void, step = 1): HTMLInputElement {
  const i = document.createElement("input");
  i.type = "number";
  i.step = String(step);
  i.value = String(Number(value.toFixed(4)));
  i.onchange = () => { const v = Number(i.value); if (Number.isFinite(v)) onChange(v); };
  return i;
}

function textInput(value: string, onChange: (v: string) => void): HTMLInputElement {
  const i = document.createElement("input");
  i.type = "text";
  i.value = value;
  i.onchange = () => onChange(i.value);
  return i;
}

function jsonInput(value: unknown, onChange: (v: unknown) => void): HTMLTextAreaElement {
  const t = document.createElement("textarea");
  t.value = JSON.stringify(value, null, 1);
  t.onchange = () => { try { onChange(JSON.parse(t.value)); t.style.borderColor = ""; } catch { t.style.borderColor = "#e5484d"; } };
  return t;
}

function varInput(spec: VarSpec, cur: unknown, onChange: (v: unknown) => void): HTMLElement {
  switch (spec.type) {
    case "number": {
      const wrap = document.createElement("div");
      wrap.className = "row";
      const r = document.createElement("input");
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
      const c = document.createElement("input");
      c.type = "color";
      c.value = String(cur).slice(0, 7);
      c.onchange = () => onChange(c.value);
      return c;
    }
    case "boolean": {
      const b = document.createElement("input");
      b.type = "checkbox";
      b.checked = Boolean(cur);
      b.onchange = () => onChange(b.checked);
      return b;
    }
    case "enum": {
      const s = document.createElement("select");
      for (const o of spec.options ?? []) { const opt = document.createElement("option"); opt.value = o; opt.textContent = o; opt.selected = o === cur; s.appendChild(opt); }
      s.onchange = () => onChange(s.value);
      return s;
    }
    case "point": {
      const p = (cur ?? { x: 0.5, y: 0.5 }) as { x: number; y: number };
      const wrap = document.createElement("div");
      wrap.className = "row";
      const x = numInput(p.x, (v) => onChange({ x: v, y: p.y }), 0.01);
      const y = numInput(p.y, (v) => onChange({ x: p.x, y: v }), 0.01);
      wrap.append(x, y);
      return wrap;
    }
    case "text": {
      const t = document.createElement("textarea");
      t.value = String(cur ?? "");
      t.onchange = () => onChange(t.value);
      return t;
    }
    default:
      return textInput(String(cur ?? ""), onChange);
  }
}
