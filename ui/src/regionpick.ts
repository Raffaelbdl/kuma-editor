// Picking a region of the picture: drag a rectangle over the preview's frame, in
// composition pixels — what `measure_color` and `sample_frames` take. A sheet over the frame
// takes the drag, so nothing under it (a live overlay, the frame's own handlers) sees it;
// Escape, or a drag too small to measure, picks nothing. View state only: a pick is not an
// edit, and it leaves nothing behind.

import { el } from "./dom";
import { store, toast } from "./store";
import type { Region } from "./types";

/** The smallest region worth measuring, in composition pixels a side. */
const MIN_SIDE = 4;

/** Ask for a region of the frame, saying `what` it is for; null when cancelled. */
export function pickRegion(what: string): Promise<Region | null> {
  const stage = document.querySelector<HTMLElement>("#preview .stage");
  const p = store.state.project;
  if (!stage || !p || !stage.offsetWidth) {
    toast("Open the preview to pick a region of the picture", "err");
    return Promise.resolve(null);
  }
  return new Promise((resolve) => {
    const box = stage.getBoundingClientRect();
    const sheet = el("div", "region-pick");
    Object.assign(sheet.style, { left: `${box.left}px`, top: `${box.top}px`, width: `${box.width}px`, height: `${box.height}px` });
    const hint = el("div", "region-pick-hint", `${what} — drag over the picture · Esc cancels`);
    const band = el("div", "region-pick-band");
    band.hidden = true;
    sheet.append(hint, band);
    document.body.appendChild(sheet);

    const sx = p.width / box.width, sy = p.height / box.height;
    /** A point on the sheet, clamped to the frame, in screen pixels from its corner. */
    const at = (e: PointerEvent) => [Math.min(box.width, Math.max(0, e.clientX - box.left)), Math.min(box.height, Math.max(0, e.clientY - box.top))];
    let from: number[] | null = null;
    let region: Region | null = null;

    const done = (r: Region | null) => {
      sheet.remove();
      window.removeEventListener("keydown", onKey, true);
      resolve(r);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      done(null);
    };
    window.addEventListener("keydown", onKey, true);

    sheet.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      // Keeps the drag when it leaves the sheet; a pointer the browser does not know (a
      // scripted one) has nothing to capture, and the sheet covers the frame anyway.
      try { sheet.setPointerCapture(e.pointerId); } catch { /* nothing to capture */ }
      from = at(e);
    });
    sheet.addEventListener("pointermove", (e) => {
      if (!from) return;
      const to = at(e);
      const [x0, x1] = [Math.min(from[0], to[0]), Math.max(from[0], to[0])];
      const [y0, y1] = [Math.min(from[1], to[1]), Math.max(from[1], to[1])];
      Object.assign(band.style, { left: `${x0}px`, top: `${y0}px`, width: `${x1 - x0}px`, height: `${y1 - y0}px` });
      band.hidden = false;
      // Whole composition pixels, inside the frame.
      const x = Math.floor(x0 * sx), y = Math.floor(y0 * sy);
      region = { x, y, w: Math.min(p.width - x, Math.round((x1 - x0) * sx)), h: Math.min(p.height - y, Math.round((y1 - y0) * sy)) };
    });
    sheet.addEventListener("pointerup", () => {
      if (!from) return;
      from = null;
      done(region && region.w >= MIN_SIDE && region.h >= MIN_SIDE ? region : null);
    });
  });
}
