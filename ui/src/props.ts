// Property editors: the small reusable controls the inspector is built from.
//
// One control per *shape* — a number, a vector, a bounded amount — so an effect field, a
// overlay var and a clip field all get the same one, and `x` is dragged the same way
// wherever you meet it. A number is a slider you can type in: drag sideways to change it,
// click without moving to put a caret in it, `Shift` for fine and `Alt` for coarse. A
// vector is one row wearing its axis colours, X red through W amber.
//
// A drag **commits once, on release**: every value here is a revision-checked patch, so
// committing per pointermove would be a round trip per pixel and an undo history of them.

import { DEAD_ZONE, dragPointer, el } from "./dom";

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
  /** Shown but not editable — a keyed value with the playhead outside its clip, where
   *  "the value now" is not a thing the panel can write to. */
  readOnly?: boolean;
  /** Hand back a setter, for a value that follows something the panel does not rebuild
   *  for: a keyframed field follows the playhead 25 times a second, and re-rendering the
   *  inspector at that rate is exactly what `store.select` exists to avoid. */
  live?: (set: (v: number) => void) => void;
  /** Called with the value as it is dragged, before it is committed: for a preview of
   *  what it would do, never an edit. */
  input?: (v: number) => void;
}

/** A value with its own field: `{ value, commit }` plus how to show and step it. */
export type NumberPart = NumberOpts & { value: number; commit: (v: number) => void };

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
export function numberField(initial: number, commit: (v: number) => void, opts: NumberOpts = {}): HTMLElement {
  const { step = 1, min, max, suffix = "", tag, axis, title, readOnly, live } = opts;
  const wrap = el("div", `num${axis ? ` axis-${axis}` : ""}${readOnly ? " off" : ""}`);
  if (title) wrap.title = title;
  if (tag) wrap.appendChild(el("span", "tag", tag));

  const input = el("input");
  input.type = "text";
  input.inputMode = "decimal";
  input.spellcheck = false;
  wrap.appendChild(input);
  if (suffix) wrap.appendChild(el("span", "suffix", suffix));

  const clamp = (v: number) => Math.min(max ?? Infinity, Math.max(min ?? -Infinity, v));
  /** What the document says. A live source moves it under us; an edit is measured against
   *  it, so a field that has not changed sends nothing. */
  let value = clamp(initial);
  /** What the field currently reads, which during a drag is ahead of the document. */
  let shown = value;
  const show = (v: number) => { shown = v; input.value = showNumber(v); };
  show(shown);
  let dragging = false;

  /** Accept whatever the field reads now. Nothing is sent when it has not moved: a click
   *  that only put a caret in a field is not an edit, and would still cost a revision. */
  const accept = () => {
    const v = clamp(Number(input.value));
    if (!Number.isFinite(v)) { show(value); return; }
    show(v);
    if (v !== value) { value = v; commit(v); }
  };

  // A value that follows the playhead, never while it is being typed in or dragged.
  live?.((v: number) => {
    if (dragging || document.activeElement === input) return;
    const next = clamp(v);
    // Nothing to do on a held segment, and this runs per playhead tick.
    if (next === value && shown === value) return;
    value = next;
    show(value);
  });

  if (readOnly) {
    input.readOnly = true;
    return wrap;
  }

  wrap.addEventListener("pointerdown", (e) => {
    // Already typing in it: the press is a caret, not a drag.
    if (e.button !== 0 || document.activeElement === input) return;
    // The press is taken outright — no focus, no text selection — and given back on
    // release if it turned out to be a click.
    const base = shown;
    dragging = true;
    dragPointer(e, {
      move: (dx, _dy, ev) => {
        if (!wrap.classList.contains("dragging")) {
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
        opts.input?.(shown);
      },
      end: (moved) => {
        wrap.classList.remove("dragging");
        document.body.classList.remove("scrubbing");
        dragging = false;
        if (moved) accept();
        else { input.focus(); input.select(); }
      },
    });
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
  // The slider follows whatever the field follows, so a keyed value moves both.
  const live = opts.live && ((set: (v: number) => void) => opts.live!((v) => {
    set(v);
    const next = String(v);
    if (document.activeElement !== range && range.value !== next) range.value = next;
  }));
  const num = numberField(value, commit, { ...opts, live, ...(opts.soft ? { min: undefined, max: undefined } : {}) });
  const input = num.querySelector("input")!;
  if (opts.readOnly) range.disabled = true;
  range.oninput = () => { input.value = showNumber(Number(range.value)); opts.input?.(Number(range.value)); };
  range.onchange = () => commit(Number(range.value));
  wrap.append(range, num);
  return wrap;
}

/**
 * The keyframe strip: where a clip's keys are in its own time, as diamonds you can click
 * and drag. Dumb on purpose — it is handed times and hands back times, so the panel and
 * the timeline can both draw one without either of them owning what a keyframe means.
 */
export interface KeyStripOpts {
  /** The span the strip covers, seconds: a clip's length. */
  duration: number;
  /** The keys within it: when, and a class saying what shape to draw (an ease, here). */
  keys: { t: number; kind?: string }[];
  /** Where the playhead is in that span, or null when it is outside. */
  playhead: number | null;
  /** Click: somewhere in the strip, or on a key. */
  onSeek: (t: number) => void;
  /** Drag: a key was let go somewhere else. */
  onMove: (i: number, t: number) => void;
  /** Double-click on empty strip. */
  onAdd?: (t: number) => void;
  /** Round a time the way the caller wants it (the frame grid, usually). */
  snap?: (t: number) => number;
  /** Hand back a setter for the playhead marker, as `NumberOpts.live` does for a value. */
  live?: (set: (playhead: number | null) => void) => void;
}

export function keyStrip(o: KeyStripOpts): HTMLElement {
  const snap = o.snap ?? ((t: number) => t);
  const wrap = el("div", "kfs");
  const lane = el("div", "kfs-lane");
  // The keys sit on a track inset from the lane's ends, or one at 0s would be half outside
  // the box it belongs to.
  const track = el("div", "kfs-track");
  const head = el("div", "kfs-head");
  const pct = (t: number) => `${Math.min(100, Math.max(0, (t / Math.max(o.duration, 1e-6)) * 100))}%`;
  const showHead = (t: number | null) => {
    head.hidden = t === null;
    if (t !== null) head.style.left = pct(t);
  };
  showHead(o.playhead);
  track.appendChild(head);

  const timeAt = (clientX: number) => {
    const r = track.getBoundingClientRect();
    return snap(Math.min(o.duration, Math.max(0, ((clientX - r.left) / r.width) * o.duration)));
  };

  o.keys.forEach(({ t, kind }, i) => {
    const key = el("div", `kfs-key${kind ? ` ${kind}` : ""}`);
    key.style.left = pct(t);
    key.title = `${t.toFixed(2)}s${kind ? ` · ${kind}` : ""} — drag to move it, click to go there`;
    key.addEventListener("pointerdown", (e) => {
      // The lane's own click is a seek; a key's is the key's.
      e.stopPropagation();
      let at = t;
      dragPointer(e, {
        move: (_dx, _dy, ev) => { at = timeAt(ev.clientX); key.style.left = pct(at); },
        end: (moved) => { if (moved && at !== t) o.onMove(i, at); else o.onSeek(t); },
      });
    });
    track.appendChild(key);
  });

  lane.addEventListener("pointerdown", (e) => o.onSeek(timeAt(e.clientX)));
  if (o.onAdd) lane.addEventListener("dblclick", (e) => o.onAdd!(timeAt(e.clientX)));
  lane.appendChild(track);
  wrap.appendChild(lane);
  o.live?.(showHead);
  return wrap;
}
