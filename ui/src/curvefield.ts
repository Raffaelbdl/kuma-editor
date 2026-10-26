// The curve field: one of a grade's curves drawn as it bends the picture, edited by its points.
//
// Drag a point to move it, press on empty space to add one there, double-click a point to
// take it out (a corner goes back to where it was). The ends move up and down only, so the
// curve always spans the range. Like every field here it commits once, on release: one
// patch per gesture, not one per pixel. What it draws is `curve.ts`, the compiler's curve.
//
// A tone curve is drawn `in → out` over the diagonal; a curve of amounts about a level line
// at none, -1 at the foot and 1 at the top; one against hue the same over the hues, and it
// has no ends — it wraps round, so its last point bends towards its first.

import { button, dragPointer, el, row } from "./dom";
import { svgEl } from "./icons";
import { corners, curveOf, hueColour, withCorners, withoutCorners, type CurveKind, type CurvePoint } from "./curve";

/** The closest two points may come, in `in`: the curve needs `in` rising. */
const GAP = 0.01;
/** The most points the model takes. */
const MOST = 32;
/** How many segments the curve is drawn with. */
const SAMPLES = 128;

const round = (v: number) => Number(v.toFixed(3));

export interface CurveFieldOpts {
  title?: string;
  /** The points as they are dragged, before the commit: a preview, never an edit. */
  input?: (points: CurvePoint[]) => void;
  kind?: CurveKind;
  /** The channel a tone curve is of, which colours its line. */
  channel?: "r" | "g" | "b";
  /** What the plot's across and up say, for the readout: `in`/`out` for a tone curve. */
  axes?: [string, string];
}

export function curveField(initial: CurvePoint[], commit: (points: CurvePoint[]) => void, opts: CurveFieldOpts = {}): HTMLElement {
  const kind = opts.kind ?? "tone";
  const [across, up] = opts.axes ?? (kind === "tone" ? ["in", "out"] : [kind === "hue" ? "hue" : "at", "amount"]);
  /** A value's height on the plot, 0..1, and back: amounts run -1..1. */
  const toPlot = (v: number) => (kind === "tone" ? v : (v + 1) / 2);
  const fromPlot = (v: number) => (kind === "tone" ? v : v * 2 - 1);
  const wrap = el("div", "curve-field");
  const plot = el("div", `curve-plot curve-${kind}`);
  if (opts.title) plot.title = opts.title;
  if (kind === "hue") {
    const stops = Array.from({ length: 13 }, (_, i) => `${hueColour(i / 12)} ${((i / 12) * 100).toFixed(1)}%`);
    plot.style.setProperty("--hues", `linear-gradient(to right, ${stops.join(", ")})`);
  }
  const svg = svgEl("svg", { viewBox: "0 0 100 100", preserveAspectRatio: "none" });
  for (const at of [25, 50, 75]) {
    svg.appendChild(svgEl("line", { x1: String(at), y1: "0", x2: String(at), y2: "100", class: "grid" }));
    svg.appendChild(svgEl("line", { x1: "0", y1: String(at), x2: "100", y2: String(at), class: "grid" }));
  }
  svg.appendChild(kind === "tone"
    ? svgEl("line", { x1: "0", y1: "100", x2: "100", y2: "0", class: "diagonal" })
    : svgEl("line", { x1: "0", y1: "50", x2: "100", y2: "50", class: "diagonal" }));
  const line = svgEl("polyline", { class: `curve${opts.channel ? ` curve-${opts.channel}` : ""}` });
  svg.appendChild(line);
  plot.appendChild(svg);
  const readout = el("span", "curve-readout muted tiny");
  const straight = button(kind === "tone" ? "Straight" : "Flat", () => { if (withoutCorners(pts, kind).length) { pts = withCorners([], kind); draw(); commit([]); } }, "Take every point out: the picture as it was");
  straight.classList.add("curve-reset");
  wrap.append(plot, row("curve-foot", readout, straight));

  /** What the field shows: the stored points with the corners they imply. */
  let pts: CurvePoint[] = withCorners(initial, kind);
  const handles: HTMLElement[] = [];
  const isEnd = (i: number) => kind !== "hue" && (i === 0 || i === pts.length - 1);
  const said = (i: number) => `${across} ${pts[i][0].toFixed(2)} → ${up} ${pts[i][1].toFixed(2)}`;

  const drawLine = () => {
    const f = curveOf(pts, kind) ?? ((v: number) => (kind === "tone" ? v : 0));
    line.setAttribute("points", Array.from({ length: SAMPLES + 1 }, (_, i) => {
      const x = i / SAMPLES;
      return `${x * 100},${(1 - Math.min(1, Math.max(0, toPlot(f(x))))) * 100}`;
    }).join(" "));
  };
  /** Put handle `i` where its point is. */
  const place = (i: number) => {
    const [x, y] = pts[i];
    Object.assign(handles[i].style, { left: `${x * 100}%`, top: `${(1 - toPlot(y)) * 100}%` });
    handles[i].title = `${said(i)} — drag to move, double-click to ${isEnd(i) ? "reset" : "remove"}`;
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
        if (isEnd(i)) pts[i] = corners(kind)[i === 0 ? 0 : 1];
        else pts.splice(i, 1);
        draw();
        commit(withoutCorners(pts, kind));
      });
      plot.appendChild(h);
      handles.push(h);
      place(i);
    });
  };

  /** Where a pointer is on the plot, as a point of the curve, the plot being at `r`. A
   *  curve that wraps stops short of 1, which is its 0 again. */
  const at = (r: DOMRect, clientX: number, clientY: number): CurvePoint =>
    [Math.min(kind === "hue" ? 1 - GAP : 1, Math.max(0, (clientX - r.left) / r.width)), fromPlot(Math.min(1, Math.max(0, 1 - (clientY - r.top) / r.height)))];

  /** Drag point `i`, committing on release if it moved. */
  const grab = (i: number, e: PointerEvent, added = false) => {
    const r = plot.getBoundingClientRect();
    const lo = isEnd(i) ? pts[i][0] : i > 0 ? pts[i - 1][0] + GAP : 0;
    const hi = isEnd(i) ? pts[i][0] : i < pts.length - 1 ? pts[i + 1][0] - GAP : 1 - GAP;
    const show = () => { readout.textContent = said(i); };
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
        opts.input?.(withoutCorners(pts, kind));
      },
      end: (moved) => {
        plot.classList.remove("dragging");
        if (moved || added) commit(withoutCorners(pts, kind));
      },
    });
  };

  plot.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    const [x, y] = at(plot.getBoundingClientRect(), e.clientX, e.clientY);
    if (withoutCorners(pts, kind).length >= MOST || pts.some(([px]) => Math.abs(px - x) < GAP)) return;
    const after = pts.findIndex(([px]) => px > x);
    const i = after < 0 ? pts.length : after;
    pts.splice(i, 0, [round(x), round(y)]);
    draw();
    grab(i, e, true);
  });

  draw();
  return wrap;
}
