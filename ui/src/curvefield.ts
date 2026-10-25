// The curve field: a grade's tone curve drawn as it bends the picture, edited by its points.
//
// Drag a point to move it, press on empty space to add one there, double-click a point to
// take it out (a corner goes back to where it was). The ends move up and down only, so the
// curve always spans black to white. Like every field here it commits once, on release: one
// patch per gesture, not one per pixel. What it draws is `curve.ts`, the compiler's curve.

import { button, dragPointer, el, row } from "./dom";
import { svgEl } from "./icons";
import { curveThrough, withCorners, withoutCorners, type CurvePoint } from "./curve";

/** The closest two points may come, in `in`: the curve needs `in` rising. */
const GAP = 0.01;
/** The most points the model takes. */
const MOST = 32;
/** How many segments the curve is drawn with. */
const SAMPLES = 128;

const round = (v: number) => Number(v.toFixed(3));

export function curveField(initial: CurvePoint[], commit: (points: CurvePoint[]) => void, opts: { title?: string } = {}): HTMLElement {
  const wrap = el("div", "curve-field");
  const plot = el("div", "curve-plot");
  if (opts.title) plot.title = opts.title;
  const svg = svgEl("svg", { viewBox: "0 0 100 100", preserveAspectRatio: "none" });
  for (const at of [25, 50, 75]) {
    svg.appendChild(svgEl("line", { x1: String(at), y1: "0", x2: String(at), y2: "100", class: "grid" }));
    svg.appendChild(svgEl("line", { x1: "0", y1: String(at), x2: "100", y2: String(at), class: "grid" }));
  }
  svg.appendChild(svgEl("line", { x1: "0", y1: "100", x2: "100", y2: "0", class: "diagonal" }));
  const line = svgEl("polyline", { class: "curve" });
  svg.appendChild(line);
  plot.appendChild(svg);
  const readout = el("span", "curve-readout muted tiny");
  const straight = button("Straight", () => { if (withoutCorners(pts).length) { pts = withCorners([]); draw(); commit([]); } }, "Take every point out: the picture as it was");
  straight.classList.add("curve-reset");
  wrap.append(plot, row("curve-foot", readout, straight));

  /** What the field shows: the stored points with the corners they imply. */
  let pts: CurvePoint[] = withCorners(initial);
  const handles: HTMLElement[] = [];
  const isEnd = (i: number) => i === 0 || i === pts.length - 1;

  const drawLine = () => {
    const f = curveThrough(pts) ?? ((v: number) => v);
    line.setAttribute("points", Array.from({ length: SAMPLES + 1 }, (_, i) => {
      const x = i / SAMPLES;
      return `${x * 100},${(1 - Math.min(1, Math.max(0, f(x)))) * 100}`;
    }).join(" "));
  };
  /** Put handle `i` where its point is. */
  const place = (i: number) => {
    const [x, y] = pts[i];
    Object.assign(handles[i].style, { left: `${x * 100}%`, top: `${(1 - y) * 100}%` });
    handles[i].title = `in ${x.toFixed(2)} → out ${y.toFixed(2)} — drag to move, double-click to ${isEnd(i) ? "reset" : "remove"}`;
  };
  /** Everything, handles made afresh: for when points come or go. A drag moves one handle
   *  and redraws the line, and leaves the rest alone. */
  const draw = () => {
    drawLine();
    for (const h of handles) h.remove();
    handles.length = 0;
    pts.forEach((_, i) => {
      const h = el("div", `curve-pt${isEnd(i) ? " end" : ""}`);
      h.addEventListener("pointerdown", (e) => { e.stopPropagation(); grab(i, e); });
      h.addEventListener("dblclick", (e) => {
        e.stopPropagation();
        if (i === 0) pts[0] = [0, 0];
        else if (i === pts.length - 1) pts[i] = [1, 1];
        else pts.splice(i, 1);
        draw();
        commit(withoutCorners(pts));
      });
      plot.appendChild(h);
      handles.push(h);
      place(i);
    });
  };

  /** Where a pointer is on the plot, 0..1 both ways, `out` up, the plot being at `r`. */
  const at = (r: DOMRect, clientX: number, clientY: number): CurvePoint =>
    [Math.min(1, Math.max(0, (clientX - r.left) / r.width)), Math.min(1, Math.max(0, 1 - (clientY - r.top) / r.height))];

  /** Drag point `i`, committing on release if it moved. */
  const grab = (i: number, e: PointerEvent, added = false) => {
    const r = plot.getBoundingClientRect();
    const lo = isEnd(i) ? pts[i][0] : pts[i - 1][0] + GAP;
    const hi = isEnd(i) ? pts[i][0] : pts[i + 1][0] - GAP;
    const show = () => { readout.textContent = `in ${pts[i][0].toFixed(2)} → out ${pts[i][1].toFixed(2)}`; };
    show();
    plot.classList.add("dragging");
    dragPointer(e, {
      axes: "xy",
      move: (dx, dy) => {
        const [x, y] = at(r, e.clientX + dx, e.clientY + dy);
        pts[i] = [round(Math.min(hi, Math.max(lo, x))), round(y)];
        show();
        place(i);
        drawLine();
      },
      end: (moved) => {
        plot.classList.remove("dragging");
        if (moved || added) commit(withoutCorners(pts));
      },
    });
  };

  plot.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    const [x, y] = at(plot.getBoundingClientRect(), e.clientX, e.clientY);
    if (withoutCorners(pts).length >= MOST || pts.some(([px]) => Math.abs(px - x) < GAP)) return;
    const i = pts.findIndex(([px]) => px > x);
    pts.splice(i, 0, [round(x), round(y)]);
    draw();
    grab(i, e, true);
  });

  draw();
  return wrap;
}
