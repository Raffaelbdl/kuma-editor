// Tiny DOM helpers. Everything user- or model-controlled goes through `textContent`,
// never `innerHTML`, so a clip label or template description cannot inject markup.

export function el<K extends keyof HTMLElementTagNameMap>(tag: K, className = "", text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

export function button(label: string, onClick: () => void, title = ""): HTMLButtonElement {
  const b = el("button", "", label);
  b.onclick = onClick;
  if (title) b.title = title;
  return b;
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
