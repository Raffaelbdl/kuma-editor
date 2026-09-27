// The seams between the panels: drag one to size the bin, the inspector or the timeline;
// double-click it to put that panel back. Sizes are view state and this browser's, kept in
// `localStorage` as the theme is — never in the document, where they would be an edit.
//
// Each size is a CSS variable on `#app` that the grid templates in `style.css` read, so
// the panels themselves know nothing of this: the preview and the workbench already
// re-fit on their own `ResizeObserver`.

import { el } from "./dom";

interface Seam {
  /** The CSS variable, and the `localStorage` key under `kuma.layout.`. */
  name: string;
  fallback: number;
  min: number;
  /** The most it may take of the window's width (or height, for a row). */
  share: number;
  axis: "x" | "y";
  /** +1 when dragging right (or down) grows the panel, -1 when it shrinks it. */
  grow: 1 | -1;
  /** Where the seam is drawn, as CSS on the handle. */
  place: Partial<CSSStyleDeclaration>;
}

const SEAMS: Seam[] = [
  { name: "--lib-w", fallback: 240, min: 160, share: 0.4, axis: "x", grow: 1, place: { left: "calc(var(--lib-w) - 3px)" } },
  { name: "--insp-w", fallback: 300, min: 220, share: 0.45, axis: "x", grow: -1, place: { right: "calc(var(--insp-w) - 3px)" } },
  { name: "--bottom-h", fallback: 300, min: 120, share: 0.75, axis: "y", grow: -1, place: { bottom: "calc(var(--bottom-h) - 3px)" } },
];

const KEY = "kuma.layout.";

const clamp = (s: Seam, v: number) => Math.round(Math.min(Math.max(s.min, v), (s.axis === "x" ? innerWidth : innerHeight) * s.share));

export function installSplitters(): void {
  const app = document.getElementById("app")!;
  const main = document.getElementById("main")!;
  const set = (s: Seam, v: number) => app.style.setProperty(s.name, `${clamp(s, v)}px`);
  for (const s of SEAMS) {
    let saved = NaN;
    try { saved = Number(localStorage.getItem(KEY + s.name)); } catch { /* no storage */ }
    set(s, saved || s.fallback);
    const handle = el("div", `splitter ${s.axis}`);
    Object.assign(handle.style, s.place);
    handle.title = "Drag to resize — double-click to reset";
    const size = () => parseFloat(getComputedStyle(app).getPropertyValue(s.name)) || s.fallback;
    const keep = () => { try { localStorage.setItem(KEY + s.name, String(size())); } catch { /* this session only */ } };
    handle.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      const from = size();
      const start = s.axis === "x" ? e.clientX : e.clientY;
      handle.classList.add("on");
      const move = (ev: PointerEvent) => set(s, from + s.grow * ((s.axis === "x" ? ev.clientX : ev.clientY) - start));
      const up = () => {
        handle.classList.remove("on");
        removeEventListener("pointermove", move);
        removeEventListener("pointerup", up);
        keep();
      };
      addEventListener("pointermove", move);
      addEventListener("pointerup", up);
    });
    handle.addEventListener("dblclick", () => { set(s, s.fallback); keep(); });
    // Columns are seams of `#main`, the row one of `#app`: each is positioned against the
    // grid it divides, and absolutely, so it takes no cell of it.
    (s.axis === "x" ? main : app).appendChild(handle);
  }
  // A window made smaller must not leave a panel wider than the share it may have.
  addEventListener("resize", () => { for (const s of SEAMS) set(s, parseFloat(app.style.getPropertyValue(s.name)) || s.fallback); });
}
