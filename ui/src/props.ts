// Property editors: the small reusable controls the inspector is built from.
//
// A video editor's parameters are a game engine's: a handful of shapes (a number, a
// vector, a bounded amount, a choice) used over and over by things that otherwise have
// nothing in common. So they live here rather than next to any one of them, and an effect,
// a template var and a clip field all get the same control for the same shape — which is
// the point of the exercise: `x` means the same thing, and is dragged the same way,
// wherever you meet it.
//
// Two ideas are borrowed from Godot's inspector, because they are what makes that panel
// quick to use:
//
//   - **A number is a slider you can type in.** Press and drag sideways to change it;
//     click without moving to put a caret in it. `Shift` is fine (a tenth of a step),
//     `Alt` is coarse (ten), and the arrow keys step it while it has focus.
//   - **A vector is one row of fields wearing their axis colours.** X is red, Y green,
//     Z blue, W amber, and a pair that is not spatial (in/out, a crop's four edges) can
//     take its own letters instead. Reading `x` off a row of identical boxes is the thing
//     the colours remove.
//
// A drag **commits once, when it is released**. Every value here is a revision-checked
// patch to the daemon, so committing per pointermove would be a round trip per pixel and
// an undo history of them; the field shows the number moving and sends one edit — the
// same bargain the timeline's clip drag makes.

import { el } from "./dom";

export type Axis = "x" | "y" | "z" | "w";

export interface NumberOpts {
  /** What one pixel of drag, or one arrow press, is worth. */
  step?: number;
  min?: number;
  max?: number;
  /** Shown after the value, greyed: `px`, `s`, `°`, `×`. */
  suffix?: string;
  /** The chip at the left of the field: `X`, `L`, `In`. */
  tag?: string;
  /** What colour that chip is. Spatial axes only — a label with no axis stays grey. */
  axis?: Axis;
  title?: string;
}

/** A value with its own field: `{ value, commit }` plus how to show and step it. */
export type NumberPart = NumberOpts & { value: number; commit: (v: number) => void };

/** Under this many pixels a press is a click (put a caret in it), not a drag. */
const DEAD_ZONE = 3;

/** Fine with Shift, coarse with Alt — the same on the drag and on the arrow keys. */
const scaleOf = (e: { shiftKey: boolean; altKey: boolean }) => (e.shiftKey ? 0.1 : e.altKey ? 10 : 1);

/** Snap to a multiple of the step *being dragged* — so a fine drag lands on fine values
 *  and a coarse one on round ones — and lose the float noise that makes a field read
 *  `0.30000000000000004`. */
const quantise = (v: number, q: number) => Number((Math.round(v / q) * q).toFixed(6));

/** What a number looks like in a field: four decimals at most, no trailing zeros. */
export const showNumber = (v: number) => (Number.isFinite(v) ? String(Number(v.toFixed(4))) : "");

/**
 * One number: drag it sideways, or click and type. `commit` is called with the new value
 * when a drag ends or an edit is accepted, and never while dragging — see the note above.
 */
export function numberField(value: number, commit: (v: number) => void, opts: NumberOpts = {}): HTMLElement {
  const { step = 1, min, max, suffix = "", tag, axis, title } = opts;
  const wrap = el("div", `num${axis ? ` axis-${axis}` : ""}`);
  if (title) wrap.title = title;
  if (tag) wrap.appendChild(el("span", "tag", tag));

  const input = el("input");
  input.type = "text";
  input.inputMode = "decimal";
  input.spellcheck = false;
  wrap.appendChild(input);
  if (suffix) wrap.appendChild(el("span", "suffix", suffix));

  const clamp = (v: number) => Math.min(max ?? Infinity, Math.max(min ?? -Infinity, v));
  /** What the field currently reads, which during a drag is ahead of the document. */
  let shown = clamp(value);
  const show = (v: number) => { shown = v; input.value = showNumber(v); };
  show(shown);

  /** Accept whatever the field reads now. Nothing is sent when it has not moved: a click
   *  that only put a caret in a field is not an edit, and would still cost a revision. */
  const accept = () => {
    const v = clamp(Number(input.value));
    if (!Number.isFinite(v)) { show(value); return; }
    show(v);
    if (v !== value) commit(v);
  };

  wrap.addEventListener("pointerdown", (e) => {
    // Already typing in it: the press is a caret, not a drag.
    if (e.button !== 0 || document.activeElement === input) return;
    // Take the press outright — no focus, no text selection — and give it back on release
    // if it turned out to be a click.
    e.preventDefault();
    const base = shown;
    const startX = e.clientX;
    let moved = false;
    const move = (ev: PointerEvent) => {
      const dx = ev.clientX - startX;
      if (!moved && Math.abs(dx) < DEAD_ZONE) return;
      if (!moved) {
        moved = true;
        wrap.classList.add("dragging");
        // The cursor belongs to the whole window while a drag is on: the pointer leaves
        // the field almost immediately and must not look like it has let go.
        document.body.classList.add("scrubbing");
      }
      // The dead zone is taken off the travel rather than added to it: without that the
      // value jumps three steps the moment a drag is recognised, which with Alt is thirty.
      const travel = dx - Math.sign(dx) * DEAD_ZONE;
      const q = step * scaleOf(ev);
      show(clamp(quantise(base + travel * q, q)));
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      wrap.classList.remove("dragging");
      document.body.classList.remove("scrubbing");
      if (moved) accept();
      else { input.focus(); input.select(); }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  });

  input.onkeydown = (e) => {
    if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      // The global shortcuts leave a field's keys alone, so these are ours to use.
      e.preventDefault();
      const q = step * scaleOf(e);
      show(clamp(quantise(shown + (e.key === "ArrowUp" ? q : -q), q)));
      accept();
    } else if (e.key === "Enter") {
      accept();
      input.blur();
    } else if (e.key === "Escape") {
      show(value);
      input.blur();
    }
  };
  input.onchange = accept;
  input.onblur = accept;
  return wrap;
}

/** Several numbers on one line, each in its own field — a position, a size, four edges.
 *
 *  Past two fields the row takes the width under its label instead of sharing it: four
 *  boxes beside a label in a 300px panel leaves about twenty pixels of number each, which
 *  is how `0.05` comes to read as `0`. */
export function vectorField(parts: NumberPart[]): HTMLElement {
  const wrap = el("div", `vec${parts.length > 2 ? " wide" : ""}`);
  for (const p of parts) wrap.appendChild(numberField(p.value, p.commit, p));
  return wrap;
}

/**
 * A bounded amount: the slider for reaching for it, the field for saying it exactly. The
 * slider commits on release for the same reason the drag does.
 *
 * `soft` means the range is what the eye uses rather than what the value may be — the
 * slider still covers min..max, but the field will take a number past either end, because
 * some of these (a contrast, a gamma) have a useful range and a legal one and they are not
 * the same range.
 */
export function sliderField(value: number, commit: (v: number) => void, opts: NumberOpts & { min: number; max: number; soft?: boolean }): HTMLElement {
  const { min, max, step = 0.01 } = opts;
  const wrap = el("div", "row");
  const range = el("input");
  range.type = "range";
  range.min = String(min);
  range.max = String(max);
  range.step = String(step);
  range.value = String(value);
  const num = numberField(value, commit, opts.soft ? { ...opts, min: undefined, max: undefined } : opts);
  const input = num.querySelector("input")!;
  range.oninput = () => { input.value = showNumber(Number(range.value)); };
  range.onchange = () => commit(Number(range.value));
  wrap.append(range, num);
  return wrap;
}
