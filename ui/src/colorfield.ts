// Colour fields: the picker a colour var or a colour entry is edited with, and the colour
// wheels a grade's lift, gamma and gain are.
//
// The picker is a swatch and its hex, opening into a saturation/value square, a hue strip
// and an alpha strip — alpha because a colour here may be `#rrggbbaa`, which the browser's
// own colour input cannot hold — and an eyedropper where the browser has one, to take a
// colour off the frame. A wheel puts a grade's per-channel offsets on a disc: the puck is
// their colour, the field under it the brightness all three share. Like every field here,
// both commit once, on release.

import { button, dragPointer, el, row } from "./dom";
import { numberField } from "./props";
import { hsvToRgb, offsetsToWheel, parseHex, rgbToHsv, toHex, wheelToOffsets, type Hsv, type Offsets } from "./color";

/** The hues round a circle, as CSS colours at 0°, 60°, … 360°: data, painted inline, so
 *  no theme moves them. */
const hues = (sat: number) => [0, 60, 120, 180, 240, 300, 360].map((h) => `hsl(${h} ${sat}% 50%)`);

/** The browser's eyedropper (Chromium: the app, and Chrome), absent elsewhere. */
interface EyeDropperApi { open(): Promise<{ sRGBHex: string }> }
const eyeDropper = (): EyeDropperApi | null => {
  const ctor = (window as unknown as { EyeDropper?: new () => EyeDropperApi }).EyeDropper;
  return ctor ? new ctor() : null;
};

/** Which pickers are open, by the key their field was given: a commit rebuilds the panel,
 *  and a picker that closed under every click could not be used. View state; nothing saves
 *  it. */
const openPickers = new Set<string>();

/** A colour: its swatch and hex, and the picker under them once the swatch is pressed.
 *  `key` names it across rebuilds of the panel (the document path it edits), so an open
 *  picker stays open. */
export function colorField(initial: string, commit: (css: string) => void, opts: { title?: string; key?: string } = {}): HTMLElement {
  const wrap = el("div", "color-field");
  const swatch = button("", () => toggle(), opts.title ?? "Pick a colour", "color-swatch");
  const chip = el("span", "color-chip");
  swatch.appendChild(chip);
  const hex = el("input");
  hex.type = "text";
  hex.spellcheck = false;
  hex.className = "color-hex mono";
  const head = row("color-head", swatch, hex);
  wrap.appendChild(head);

  /** What the document says; an edit is measured against it, so no change sends nothing. */
  let value = initial;
  let a = parseHex(initial)?.a ?? 255;
  let hsv: Hsv = rgbToHsv(parseHex(initial) ?? { r: 0, g: 0, b: 0, a: 255 });
  const current = () => toHex(hsvToRgb(hsv, a));
  const accept = () => {
    const next = current();
    if (next !== value) { value = next; commit(next); }
  };

  let pop: HTMLElement | null = null;
  let paint = () => {};
  const show = () => {
    const c = current();
    chip.style.background = c;
    if (document.activeElement !== hex) hex.value = c;
    paint();
  };

  hex.onchange = () => {
    const c = parseHex(hex.value);
    if (!c) { hex.value = value; return; }
    hsv = rgbToHsv(c);
    a = c.a;
    show();
    accept();
  };
  hex.onkeydown = (e) => { if (e.key === "Enter") hex.blur(); else if (e.key === "Escape") { hex.value = value; hex.blur(); } };

  const toggle = () => {
    if (pop) {
      pop.remove();
      pop = null;
      paint = () => {};
      wrap.classList.remove("open");
      if (opts.key) openPickers.delete(opts.key);
      return;
    }
    if (opts.key) openPickers.add(opts.key);
    pop = picker();
    wrap.appendChild(pop);
    wrap.classList.add("open");
    show();
  };

  /** A strip or square that sets values from where it is pressed, committing on release. */
  const area = (cls: string, set: (fx: number, fy: number) => void) => {
    const box = el("div", cls);
    const knob = el("span", "color-knob");
    box.appendChild(knob);
    box.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      const r = box.getBoundingClientRect();
      const put = (x: number, y: number) => {
        set(Math.min(1, Math.max(0, (x - r.left) / r.width)), Math.min(1, Math.max(0, (y - r.top) / r.height)));
        show();
      };
      put(e.clientX, e.clientY);
      dragPointer(e, { axes: "xy", move: (dx, dy) => put(e.clientX + dx, e.clientY + dy), end: () => accept() });
    });
    return { box, knob };
  };

  const picker = () => {
    const p = el("div", "color-pop");
    const sv = area("color-sv", (fx, fy) => { hsv = { ...hsv, s: fx, v: 1 - fy }; });
    const hue = area("color-hue", (fx) => { hsv = { ...hsv, h: fx * 360 }; });
    const alpha = area("color-alpha", (fx) => { a = Math.round(fx * 255); });
    // The hues, painted inline: the stops are data, not theme colours.
    hue.box.style.background = `linear-gradient(to right, ${hues(100).join(", ")})`;
    p.append(sv.box, hue.box, alpha.box);
    const drop = eyeDropper();
    if (drop) {
      p.appendChild(button("Pick from screen", async () => {
        try {
          const { sRGBHex } = await eyeDropper()!.open();
          const c = parseHex(sRGBHex);
          if (!c) return;
          hsv = rgbToHsv(c);
          show();
          accept();
        } catch { /* Escape: nothing picked */ }
      }, "Take a colour from anywhere on screen — the frame, a reference image", "color-drop"));
    }
    paint = () => {
      const pure = toHex(hsvToRgb({ h: hsv.h, s: 1, v: 1 }));
      sv.box.style.background = `linear-gradient(to top, #000, transparent), linear-gradient(to right, #fff, ${pure})`;
      Object.assign(sv.knob.style, { left: `${hsv.s * 100}%`, top: `${(1 - hsv.v) * 100}%` });
      hue.knob.style.left = `${(hsv.h / 360) * 100}%`;
      const solid = toHex(hsvToRgb(hsv));
      alpha.box.style.setProperty("--alpha-to", solid);
      alpha.knob.style.left = `${(a / 255) * 100}%`;
    };
    return p;
  };

  show();
  if (opts.key && openPickers.has(opts.key)) toggle();
  return wrap;
}

/** How far from the centre the rim of a wheel is, in offset units: past a quarter is a
 *  strong grade, and finer work wants the room. */
const RIM = 0.25;

/** A grade's wheel: its offsets' colour as a puck on a disc, and the brightness all three
 *  share as a number under it. Double-click the disc to take the colour out. */
export function wheelField(label: string, initial: Offsets, commit: (o: Offsets) => void, opts: { title?: string } = {}): HTMLElement {
  const wrap = el("div", "wheel-field");
  const disc = el("div", "wheel-disc");
  if (opts.title) disc.title = opts.title;
  // Red to the right, and round against the clock as `offsetsToWheel` puts them: CSS turns
  // clockwise, so the hues run backwards. Greyed towards the centre, where there is none.
  disc.style.background = `radial-gradient(circle closest-side, var(--bg-2), transparent), conic-gradient(from 90deg, ${hues(70).reverse().join(", ")})`;
  const puck = el("span", "wheel-puck");
  disc.appendChild(puck);
  let o: Offsets = { ...initial };
  const place = () => {
    const { x, y } = offsetsToWheel(o);
    // On the disc, `y` is up; the rim is RIM, and a puck past it waits at the edge.
    const len = Math.hypot(x, y), k = len > RIM ? RIM / len : 1;
    Object.assign(puck.style, { left: `${50 + (x * k / RIM) * 50}%`, top: `${50 - (y * k / RIM) * 50}%` });
  };
  const clamp = (v: number) => Number(Math.min(1, Math.max(-1, v)).toFixed(3));
  const set = (next: Offsets) => { o = { r: clamp(next.r), g: clamp(next.g), b: clamp(next.b) }; place(); };

  disc.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    const r = disc.getBoundingClientRect();
    const before = o;
    const put = (cx: number, cy: number) => {
      const x = ((cx - r.left) / r.width - 0.5) * 2 * RIM;
      const y = -((cy - r.top) / r.height - 0.5) * 2 * RIM;
      set(wheelToOffsets(x, y, offsetsToWheel(before).master));
    };
    put(e.clientX, e.clientY);
    disc.classList.add("dragging");
    dragPointer(e, {
      axes: "xy",
      move: (dx, dy) => put(e.clientX + dx, e.clientY + dy),
      end: () => {
        disc.classList.remove("dragging");
        if (o.r !== before.r || o.g !== before.g || o.b !== before.b) commit(o);
      },
    });
  });
  disc.addEventListener("dblclick", () => {
    const { master } = offsetsToWheel(o);
    set({ r: master, g: master, b: master });
    commit(o);
  });

  const master = numberField(offsetsToWheel(o).master, (m) => {
    const w = offsetsToWheel(o);
    set(wheelToOffsets(w.x, w.y, m));
    commit(o);
  }, { step: 0.005, min: -1, max: 1, tag: "Y", title: `${label}: the brightness all three channels share` });
  wrap.append(el("div", "wheel-label", label), disc, master);
  place();
  return wrap;
}
