// Panels and the containers they sit in.
//
// Every panel is one module — the bin, the inspector, the timeline, the agent, the
// history, the pins, the render queue — and the same module whichever container holds it:
// **docked** round the frame in power mode, **floating** over it (den, or the agent in
// power), or a **window** of its own (`?panel=<id>`). Only the container changes, so a panel
// is written once and knows nothing about where it is.
//
// All of it is view state. Which mode, which panels are summoned and where they float are
// this browser's, in `localStorage`, per mode — never the document, and never an edit: a
// layout is not something a render or an agent reads.

import { Cmd, command } from "./commands";
import { button, dragPointer, el } from "./dom";
import { desktop } from "./desktop";
import { store, toast } from "./store";

export type Mode = "power" | "den";
type Dock = "side" | "right" | "bottom";

export interface PanelSpec {
  id: string;
  /** Its key and its tile: `b`, `i`, `t`… */
  letter: string;
  title: string;
  /** The element the panel draws into. It moves between containers; it is never rebuilt. */
  body: HTMLElement;
  /** Controls for the head (the bin's + and import), owned and refreshed by the panel. */
  tools?: HTMLElement;
  /** A note after the title, quieter (the agent's client, the timeline's sequence). */
  note?: HTMLElement;
  /** Where power docks it; null floats it there too. */
  dock: Dock | null;
  /** Whether the head shows while docked. The inspector and the timeline have none. */
  head?: boolean;
  /** Its size when it first floats. */
  size?: { w: number; h: number };
  /** Called whenever it is put on screen: the bin and the blueprints share one body. */
  show?(): void;
}

/** Where a floating panel is, in the floating layer's pixels, and which edge it is held
 *  against: within `SNAP` of one it docks there and follows it as the window resizes. */
interface Geom { x: number; y: number; w: number; h: number; edge?: "left" | "right" | "bottom" }
interface Layout { open: string[]; geom: Record<string, Geom>; rail: string }

const SNAP = 24;
const KEY = "kuma.panels.";
const MODE_KEY = "kuma.mode";

interface Frame { root: HTMLElement; body: HTMLElement; spec: PanelSpec }

const read = <T>(key: string, fallback: T): T => {
  try { return { ...fallback, ...JSON.parse(localStorage.getItem(key) ?? "{}") }; } catch { return fallback; }
};
const write = (key: string, v: unknown) => { try { localStorage.setItem(key, typeof v === "string" ? v : JSON.stringify(v)); } catch { /* this session only */ } };

class PanelHost {
  private specs = new Map<string, PanelSpec>();
  private frames = new Map<string, Frame>();
  private layouts: Record<Mode, Layout> = {
    power: read<Layout>(KEY + "power", { open: [], geom: {}, rail: "bin" }),
    den: read<Layout>(KEY + "den", { open: [], geom: {}, rail: "bin" }),
  };
  /** Panels in windows of their own. While one is out it is parked here. */
  private out = new Set<string>();
  /** A window opened from this page, by panel: a browser's handle, polled for closing. */
  private windows = new Map<string, Window>();
  private layer = () => document.getElementById("floating")!;
  /** In a window of its own (`?panel=`), the page is that panel and nothing else. */
  solo: string | null = null;

  /** The mode remembered, before anything is drawn. */
  initialMode(): Mode {
    let m: string | null = null;
    try { m = localStorage.getItem(MODE_KEY); } catch { /* the default */ }
    return m === "den" ? "den" : "power";
  }

  register(spec: PanelSpec): void {
    this.specs.set(spec.id, spec);
  }

  spec(id: string): PanelSpec | undefined {
    return this.specs.get(id);
  }

  get all(): PanelSpec[] {
    return [...this.specs.values()];
  }

  get rail(): string {
    return this.layouts.power.rail;
  }

  /** Whether `id` is on screen in this window, in any container. */
  isShown(id: string): boolean {
    return !!this.frames.get(id)?.root.isConnected && this.frames.get(id)!.root.parentElement?.id !== "parking";
  }

  isOut(id: string): boolean {
    return this.out.has(id);
  }

  isFloating(id: string): boolean {
    return this.layouts[store.state.mode].open.includes(id) && !this.out.has(id);
  }

  setMode(mode: Mode): void {
    if (mode === store.state.mode) return;
    write(MODE_KEY, mode);
    store.set({ mode });
    this.apply();
  }

  /**
   * What a panel's letter does. In den it summons the panel (or sends it away). In power it
   * puts it in front: the rail's section for the side panels, the inspector and timeline
   * where they are docked, and the rest — the agent — floating.
   */
  toggle(id: string): void {
    const spec = this.specs.get(id);
    if (!spec) return;
    // Out in a window of its own: the letter calls it back. Focusing that window instead
    // would do nothing at all if it had gone without saying so, and the panel would be
    // unreachable until a reload.
    if (this.out.has(id)) { this.recall(id); return; }
    const mode = store.state.mode;
    if (mode === "power" && spec.dock === "side") {
      this.layouts.power.rail = id;
      this.save("power");
      this.apply();
      this.focusBody(id);
      return;
    }
    if (mode === "power" && spec.dock) {
      this.focusBody(id);
      return;
    }
    this.setFloating(id, !this.isFloating(id));
  }

  /** Summon `id` floating, or send it away. Two panels sharing a body (bin, blueprints)
   *  cannot both be up, so summoning one sends the other away. */
  setFloating(id: string, open: boolean): void {
    const l = this.layouts[store.state.mode];
    const body = this.specs.get(id)?.body;
    l.open = l.open.filter((x) => x !== id && (!open || this.specs.get(x)?.body !== body));
    if (open) l.open.push(id);
    this.save(store.state.mode);
    this.apply();
  }

  /** Make sure `id` is up, without toggling it off when it already is. */
  show(id: string): void {
    const spec = this.specs.get(id);
    if (!spec || this.out.has(id)) return;
    if (store.state.mode === "power" && spec.dock === "side") {
      if (this.layouts.power.rail !== id) this.toggle(id);
    } else if (!(store.state.mode === "power" && spec.dock) && !this.isFloating(id)) {
      this.setFloating(id, true);
    }
  }

  private focusBody(id: string) {
    const body = this.specs.get(id)?.body;
    if (!body) return;
    body.scrollIntoView({ block: "nearest" });
    const frame = this.frames.get(id)?.root;
    frame?.classList.add("flash");
    setTimeout(() => frame?.classList.remove("flash"), 400);
  }

  private save(mode: Mode) {
    write(KEY + mode, this.layouts[mode]);
  }

  /** Put every panel where the mode says, parking the rest. Called on every change of mode,
   *  rail, summons or windows — it is cheap: nothing is rebuilt, elements only move. */
  apply(): void {
    if (this.solo) return;
    const mode = store.state.mode;
    const app = document.getElementById("app")!;
    app.classList.toggle("den", mode === "den");
    app.classList.toggle("power", mode === "power");
    const parking = document.getElementById("parking")!;
    const l = this.layouts[mode];

    const docked: Partial<Record<Dock, string>> = {};
    if (mode === "power") {
      const side = this.specs.get(this.layouts.power.rail)?.dock === "side" ? this.layouts.power.rail : "bin";
      if (!this.out.has(side)) docked.side = side;
      for (const s of this.specs.values()) if ((s.dock === "right" || s.dock === "bottom") && !this.out.has(s.id)) docked[s.dock] = s.id;
    }
    const floating = l.open.filter((id) => this.specs.has(id) && !this.out.has(id) && !Object.values(docked).includes(id));

    // Park first: a body two panels share goes to whichever is placed after.
    for (const [id, f] of this.frames) {
      if (!Object.values(docked).includes(id) && !floating.includes(id) && f.root.parentElement !== parking) parking.appendChild(f.root);
    }
    for (const dock of ["side", "right", "bottom"] as Dock[]) {
      const slot = document.getElementById(dock)!;
      const id = docked[dock];
      app.classList.toggle(`no-${dock}`, mode === "power" && !id);
      if (!id) { slot.replaceChildren(); continue; }
      const f = this.frame(id);
      f.root.className = `panel docked${f.spec.head === false ? " nohead" : ""}`;
      f.root.removeAttribute("style");
      if (f.root.parentElement !== slot) slot.replaceChildren(f.root);
      this.attach(f);
    }
    const layer = this.layer();
    for (const id of floating) {
      const f = this.frame(id);
      f.root.className = "panel floating";
      if (f.root.parentElement !== layer) layer.appendChild(f.root);
      this.attach(f);
      this.place(f, l.geom[id] ?? this.defaultGeom(f.spec, floating.indexOf(id)));
    }
    document.dispatchEvent(new CustomEvent("kuma:panels"));
  }

  /** The body goes in the frame on screen; `show` lets a shared body set itself up. */
  private attach(f: Frame) {
    if (f.spec.body.parentElement !== f.body) f.body.replaceChildren(f.spec.body);
    if (f.spec.tools && f.spec.tools.parentElement !== f.root.querySelector(".ptools")) f.root.querySelector(".ptools")!.appendChild(f.spec.tools);
    f.spec.show?.();
  }

  private defaultGeom(spec: PanelSpec, i: number): Geom {
    const layer = this.layer().getBoundingClientRect();
    const w = spec.size?.w ?? 300, h = Math.min(spec.size?.h ?? 340, layer.height - 40);
    // Down the left then the right, so two summoned panels do not land on each other.
    if (spec.id === "timeline") return { x: 30, y: Math.max(20, layer.height - h - 20), w: Math.min(w, layer.width - 60), h };
    const right = i % 2 === 1;
    // Below den's HUD (and power's top bar), which are what tell you where you are.
    return { x: right ? layer.width - w - 28 : 28, y: 68 + Math.floor(i / 2) * 40, w, h };
  }

  private place(f: Frame, g: Geom) {
    const layer = this.layer().getBoundingClientRect();
    const w = Math.min(g.w, layer.width), h = Math.min(g.h, layer.height);
    let x = Math.min(Math.max(0, g.x), Math.max(0, layer.width - w));
    let y = Math.min(Math.max(0, g.y), Math.max(0, layer.height - h));
    let width = w;
    if (g.edge === "left") x = 0;
    if (g.edge === "right") x = layer.width - w;
    if (g.edge === "bottom") { x = 0; y = layer.height - h; width = layer.width; }
    Object.assign(f.root.style, { left: `${x}px`, top: `${y}px`, width: `${width}px`, height: `${h}px` });
    f.root.dataset.edge = g.edge ?? "";
  }

  private frame(id: string): Frame {
    let f = this.frames.get(id);
    if (f) return f;
    const spec = this.specs.get(id)!;
    const root = el("div", "panel");
    root.dataset.panel = id;
    const head = el("div", "panel-head");
    const tile = el("span", "ptile", spec.letter);
    const title = el("span", "ptitle", spec.title);
    const tools = el("span", "ptools");
    const pop = button("⧉", () => this.popOut(id), "Open in a window of its own (or drag it past the window's edge)", "pbtn pop");
    const close = button("×", () => this.solo ? this.leave() : this.setFloating(id, false), "Close", "pbtn pfloat-only");
    head.append(tile, title, ...(spec.note ? [spec.note] : []), el("span", "spacer"), tools, pop, close);
    if (spec.note) spec.note.classList.add("pnote");
    const body = el("div", "panel-body");
    const grip = el("div", "pgrip");
    root.append(head, body, grip);
    f = { root, body, spec };
    this.frames.set(id, f);
    this.dragging(f, head);
    this.resizing(f, grip);
    return f;
  }

  /** Drag by the title bar. Within `SNAP` of an edge it docks to it; against the window's
   *  edge it says what letting go past it does; past it, it becomes a window. */
  private dragging(f: Frame, head: HTMLElement) {
    head.addEventListener("pointerdown", (e) => {
      if (e.button !== 0 || !f.root.classList.contains("floating") || (e.target as HTMLElement).closest("button, input, select")) return;
      const layer = this.layer().getBoundingClientRect();
      const start = f.root.getBoundingClientRect();
      const g: Geom = { x: start.left - layer.left, y: start.top - layer.top, w: start.width, h: start.height };
      let outside = false;
      let hint: HTMLElement | null = null;
      f.root.classList.add("dragging");
      dragPointer(e, {
        axes: "xy",
        move: (dx, dy, ev) => {
          g.x = start.left - layer.left + dx;
          g.y = start.top - layer.top + dy;
          g.edge = undefined;
          this.place(f, g);
          outside = ev.clientX <= 0 || ev.clientY <= 0 || ev.clientX >= innerWidth - 1 || ev.clientY >= innerHeight - 1;
          const side = g.x <= 0 ? "left" : g.x + g.w >= layer.width ? "right" : null;
          if (side && !hint) {
            hint = el("div", `edge-hint ${side}`);
            hint.appendChild(el("span", "", "drag past the edge → window of its own"));
            hint.style[side] = "0";
            this.layer().appendChild(hint);
          } else if (!side && hint) { hint.remove(); hint = null; }
        },
        end: (moved) => {
          hint?.remove();
          f.root.classList.remove("dragging");
          if (!moved) return;
          if (outside) { this.popOut(f.spec.id); return; }
          // Snapped: flush with the edge it was dropped near, and kept there.
          if (g.x < SNAP) g.edge = "left";
          else if (layer.width - (g.x + g.w) < SNAP) g.edge = "right";
          else if (layer.height - (g.y + g.h) < SNAP && f.spec.id === "timeline") g.edge = "bottom";
          this.layouts[store.state.mode].geom[f.spec.id] = g;
          this.place(f, g);
          this.save(store.state.mode);
        },
      });
    });
  }

  private resizing(f: Frame, grip: HTMLElement) {
    grip.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      const r = f.root.getBoundingClientRect();
      const layer = this.layer().getBoundingClientRect();
      const g: Geom = { ...(this.layouts[store.state.mode].geom[f.spec.id] ?? { x: r.left - layer.left, y: r.top - layer.top, w: r.width, h: r.height }) };
      dragPointer(e, {
        axes: "xy",
        move: (dx, dy) => {
          g.w = Math.max(220, r.width + dx);
          g.h = Math.max(120, r.height + dy);
          if (g.edge === "bottom") g.edge = undefined;
          this.place(f, { ...g, x: r.left - layer.left, y: r.top - layer.top });
        },
        end: (moved) => {
          if (!moved) return;
          this.layouts[store.state.mode].geom[f.spec.id] = { ...g, x: r.left - layer.left, y: r.top - layer.top };
          this.save(store.state.mode);
        },
      });
    });
  }

  /**
   * A panel in a window of its own — a second screen's worth of timeline, say. A browser
   * opens a popup on `?panel=<id>`; the desktop shell opens a frameless window through its
   * bridge. Either way it is this same page, showing that panel alone, kept in step with
   * this one by `viewsync.ts`. While it is out, this window parks it; closing that window
   * brings it back.
   */
  popOut(id: string): void {
    if (this.solo) return;
    const spec = this.specs.get(id);
    if (!spec || this.out.has(id)) return;
    const r = (this.isShown(id) ? this.frames.get(id)!.root.getBoundingClientRect() : null);
    const w = Math.round(Math.max(r?.width ?? 0, spec.size?.w ?? 360)), h = Math.round(Math.max(r?.height ?? 0, spec.size?.h ?? 480));
    const url = `${location.pathname}?panel=${encodeURIComponent(id)}`;
    const app = desktop();
    if (app?.openPanel) {
      void app.openPanel(id, { width: w, height: h });
    } else {
      const win = window.open(url, `kuma-panel-${id}`, `popup,width=${w},height=${h},left=${screenX + (r?.left ?? 80)},top=${screenY + (r?.top ?? 80)}`);
      if (!win) { toast("The browser blocked the window: allow pop-ups for the editor and try again", "err", 8000); return; }
      this.windows.set(id, win);
      const poll = window.setInterval(() => { if (win.closed) { clearInterval(poll); this.returned(id); } }, 800);
    }
    this.out.add(id);
    this.outSince.set(id, Date.now());
    this.apply();
  }

  /** A panel's window closed (or said so): it comes back where it was — summoned, in den,
   *  whether or not it was before it went out, since putting it back is asking for it. */
  returned(id: string): void {
    this.windows.delete(id);
    this.outSince.delete(id);
    if (!this.out.delete(id)) return;
    const spec = this.specs.get(id);
    const l = this.layouts[store.state.mode];
    if (spec && !(store.state.mode === "power" && spec.dock) && !l.open.includes(id)) {
      const body = spec.body;
      l.open = l.open.filter((x) => this.specs.get(x)?.body !== body).concat(id);
      this.save(store.state.mode);
    }
    this.apply();
  }

  /** Bring a panel back from its window, closing the window if it is still there. */
  recall(id: string): void {
    this.onRecall(id);
    this.windows.get(id)?.close();
    this.returned(id);
  }

  /** How the editor window asks a panel's window to close (`viewsync.ts` sets it). */
  onRecall: (id: string) => void = () => {};
  /** How a panel's window leaves: told to the editor window first, then closed. */
  leave: () => void = () => window.close();

  /** When each panel went out, for the heartbeat's grace (`viewsync.ts`). */
  readonly outSince = new Map<string, number>();
  get outIds(): string[] {
    return [...this.out];
  }

  /** Told that a panel is in a window (a window opened before this page reloaded). */
  claimed(id: string): void {
    if (!this.specs.has(id)) return;
    this.outSince.set(id, Date.now());
    if (this.out.has(id)) return;
    this.out.add(id);
    this.apply();
  }

  /** This page is a panel's own window: show that panel, full size, and nothing else. */
  mountSolo(id: string): boolean {
    const spec = this.specs.get(id);
    if (!spec) return false;
    this.solo = id;
    document.body.classList.add("solo");
    const f = this.frame(id);
    f.root.className = "panel solo";
    // Its ⧉ means "back into the editor", which is closing this window.
    const pop = f.root.querySelector<HTMLButtonElement>(".pop")!;
    pop.textContent = "↙";
    pop.title = "Put it back in the editor window";
    pop.onclick = () => this.leave();
    this.layer().appendChild(f.root);
    this.attach(f);
    document.title = `${spec.title} — kuma`;
    return true;
  }
}

export const panels = new PanelHost();

/** The letters, as commands: ⌘K, the menus and the keys all reach a panel the same way. */
export const PANEL_KEYS: [letter: string, id: string][] = [
  ["b", "bin"], ["i", "inspector"], ["t", "timeline"], ["a", "agent"], ["h", "history"], ["m", "pins"], ["r", "queue"],
];

export function panelCommands(): void {
  for (const s of panels.all) command(`panel.${s.id}`, `Panel: ${s.title}`, () => panels.toggle(s.id));
  command(Cmd.modeDen, "Den mode", () => panels.setMode("den"));
  command(Cmd.modePower, "Power mode", () => panels.setMode("power"));
  command(Cmd.toggleMode, "Switch den / power", () => panels.setMode(store.state.mode === "den" ? "power" : "den"));
  for (const s of panels.all) command(`panel.${s.id}.window`, `Window: ${s.title}`, () => panels.popOut(s.id));
  // The window may resize under floating panels: keep them on screen and on their edges.
  addEventListener("resize", () => panels.apply());
}
