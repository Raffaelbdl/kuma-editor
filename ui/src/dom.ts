// Tiny DOM helpers. Everything user- or model-controlled goes through `textContent`,
// never `innerHTML`, so a clip label or overlay description cannot inject markup.

export function el<K extends keyof HTMLElementTagNameMap>(tag: K, className = "", text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** A button labelled with text, or with an icon node (see `icons.ts`). */
export function button(label: string | Node, onClick: () => void, title = "", className = ""): HTMLButtonElement {
  const b = el("button", className);
  b.append(label);
  b.onclick = onClick;
  if (title) b.title = title;
  return b;
}

/** A div of `className` holding `kids` — the one-line "put these in a row". */
export function row(className: string, ...kids: (HTMLElement | string)[]): HTMLDivElement {
  const r = el("div", className);
  r.append(...kids);
  return r;
}

export function checkbox(checked: boolean, onChange?: (v: boolean) => void): HTMLInputElement {
  const c = el("input");
  c.type = "checkbox";
  c.checked = checked;
  if (onChange) c.onchange = () => onChange(c.checked);
  return c;
}

/** `3 clips`, `1 entry`, `2 entries`. */
export const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** `brightness` → `Brightness`: a key or a kind as a label. */
export const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/** What this browser keeps for the editor (`localStorage`). A private window has no
 *  storage and throws on access, which must cost only the remembering: null then, and a
 *  write that keeps nothing beyond this session. */
export const loadLocal = (key: string): string | null => { try { return localStorage.getItem(key); } catch { return null; } };
export const saveLocal = (key: string, v: string): void => { try { localStorage.setItem(key, v); } catch { /* this session only */ } };
/** A kept object, laid over `fallback` so a field added since it was kept has its default. */
export function loadLocalJSON<T extends object>(key: string, fallback: T): T {
  try { return { ...fallback, ...JSON.parse(loadLocal(key) ?? "{}") }; } catch { return fallback; }
}

/** Under this many pixels a press is a click, not a drag. */
export const DEAD_ZONE = 3;

/**
 * The press-drag-or-click gesture, from a `pointerdown`. The window's `pointermove` and
 * `pointerup` are taken for the duration and let go on release; `move` gets the travel so
 * far and `end` whether there was any, so a press under the dead zone is a click rather
 * than a drag of nothing. The press itself is taken (no focus, no text selection) unless
 * the caller says otherwise, and only `dx` counts towards the dead zone unless `axes`
 * says both do.
 */
export function dragPointer(e: PointerEvent, h: { move: (dx: number, dy: number, ev: PointerEvent) => void; end: (moved: boolean) => void; axes?: "x" | "xy"; keepDefault?: boolean }): void {
  if (!h.keepDefault) e.preventDefault();
  const startX = e.clientX;
  const startY = e.clientY;
  let moved = false;
  const move = (ev: PointerEvent) => {
    const dx = ev.clientX - startX;
    const dy = ev.clientY - startY;
    if (!moved && Math.abs(dx) < DEAD_ZONE && (h.axes !== "xy" || Math.abs(dy) < DEAD_ZONE)) return;
    moved = true;
    h.move(dx, dy, ev);
  };
  const up = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    h.end(moved);
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
}

/** A `<select>` with its change handler, the way `textInput` wraps its own. */
export function selectInput(items: { value: string; label?: string; title?: string }[], selected: string | undefined, onChange: (v: string) => void, placeholder?: string): HTMLSelectElement {
  const s = el("select");
  options(s, items, selected, placeholder);
  s.onchange = () => onChange(s.value);
  return s;
}

/** The one drag payload the editor uses: a bin entry, or a blueprint not yet in the project. */
export interface DragPayload { bin?: string; blueprint?: string }
const DRAG_TYPE = "application/x-kuma";
export function setDragPayload(e: DragEvent, payload: DragPayload): void {
  e.dataTransfer?.setData(DRAG_TYPE, JSON.stringify(payload));
}
export function dragPayload(e: DragEvent): DragPayload | null {
  const data = e.dataTransfer?.getData(DRAG_TYPE);
  try { return data ? (JSON.parse(data) as DragPayload) : null; } catch { return null; }
}

/** Whether a drag may carry files from outside the page. One with no types counts: GNOME on
 *  Wayland often sends a file drag with none, and every drag the page starts has one. */
export const isFileDrag = (e: DragEvent): boolean => {
  const types = e.dataTransfer?.types;
  return !!types && (types.length === 0 || types.includes("Files"));
};

/** Fill a `<select>` with options (value, label, tooltip); keeps a leading placeholder if given. */
export function options(sel: HTMLSelectElement, items: { value: string; label?: string; title?: string }[], selected?: string, placeholder?: string) {
  sel.innerHTML = "";
  if (placeholder !== undefined) sel.appendChild(new Option(placeholder, ""));
  for (const it of items) {
    const o = new Option(it.label ?? it.value, it.value, false, it.value === selected);
    if (it.title) o.title = it.title;
    sel.appendChild(o);
  }
}
