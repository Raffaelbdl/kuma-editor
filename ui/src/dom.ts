// Tiny DOM helpers. Everything user- or model-controlled goes through `textContent`,
// never `innerHTML`, so a clip label or template description cannot inject markup.

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

/** A `<select>` with its change handler, the way `numInput`/`textInput` wrap theirs. */
export function selectInput(items: { value: string; label?: string; title?: string }[], selected: string | undefined, onChange: (v: string) => void, placeholder?: string): HTMLSelectElement {
  const s = el("select");
  options(s, items, selected, placeholder);
  s.onchange = () => onChange(s.value);
  return s;
}

/** The one drag payload the editor uses: a bin entry, or a template not yet in the bin. */
export interface DragPayload { bin?: string; template?: string }
const DRAG_TYPE = "application/x-kuma";
export function setDragPayload(e: DragEvent, payload: DragPayload): void {
  e.dataTransfer?.setData(DRAG_TYPE, JSON.stringify(payload));
}
export function dragPayload(e: DragEvent): DragPayload | null {
  const data = e.dataTransfer?.getData(DRAG_TYPE);
  try { return data ? (JSON.parse(data) as DragPayload) : null; } catch { return null; }
}

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
