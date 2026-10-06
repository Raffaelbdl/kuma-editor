// Panels and the containers they sit in.
//
// Every panel is one module — the bin, the inspector, the timeline, the agent, the
// history, the pins, the render queue — and the same module whichever container holds it:
// a **group** in power's dock (`dock.ts`: splits and tabs round the frame, rearranged by
// dragging a tab), **floating** over the frame (den, or power when torn off), or a
// **window** of its own (`?panel=<id>`). Only the container changes, so a panel is written
// once and knows nothing about where it is.
//
// All of it is view state. The dock, which panels float and where, are this browser's, in
// `localStorage` — never the document, and never an edit: a layout is not something a
// render or an agent reads.

import { Cmd, command } from "./commands";
import { button, dragPointer, el, loadLocal, loadLocalJSON, saveLocal } from "./dom";
import { desktop } from "./desktop";
import { defaultTree, find, type Homes, fillOf, groupOf, hasViewer, insert, panelsIn, remove, sanitize, type Group, type Node, type Split, type Target } from "./dock";
import { showMenu, type MenuItem } from "./menu";
import { store, toast, type State } from "./store";

export type Mode = State["mode"];
type Home = "side" | "right" | "bottom";

export interface PanelSpec {
  id: string;
  /** Its tile: `b`, `i`, `t`… */
  letter: string;
  /** What its letter does, in the shortcut list — set when the letter is also its key (`B I
   *  T A H M R`). A panel without one (blueprints, fonts) has a tile and no key. */
  key?: string;
  title: string;
  /** The element the panel draws into. It moves between containers; it is never rebuilt. */
  body: HTMLElement;
  /** Controls for the head (the bin's + and import), owned and refreshed by the panel. */
  tools?: HTMLElement;
  /** A note after the title, quieter (the agent's client, the timeline's sequence). */
  note?: HTMLElement;
  /** Where it goes in the dock when it is asked for and has no group to go back to; null
   *  floats it instead (the agent). */
  dock: Home | null;
  /** Its size when it first floats, or first gets a group of its own. */
  size?: { w: number; h: number };
  /** Called whenever it is put on screen. */
  show?(): void;
}

/** Where a floating panel is, in the floating layer's pixels, and which edge it is held
 *  against: within `SNAP` of one it sticks there and follows it as the window resizes. */
interface Geom { x: number; y: number; w: number; h: number; edge?: "left" | "right" | "bottom" }
interface Floating { open: string[]; geom: Record<string, Geom> }

const SNAP = 24;
/** The least a floating panel can be resized to and still be used. */
const MIN_W = 220;
const MIN_H = 120;
/** How close to the dock's own edge a drop splits the whole dock rather than a group. */
const ROOT_EDGE = 26;
const KEY = "kuma.panels.";
const DOCK_KEY = "kuma.dock";
const MODE_KEY = "kuma.mode";
const MOVABLE_KEY = "kuma.dock.movable";

/** A panel's own container, for when it floats or has a window: its head's slots kept. */
interface Frame { root: HTMLElement; body: HTMLElement; spec: PanelSpec; tools: HTMLElement; note: HTMLElement; pop: HTMLButtonElement }
/** A group's elements, kept across draws so nothing on screen is rebuilt. */
interface GroupEls { root: HTMLElement; tabs: HTMLElement; note: HTMLElement; tools: HTMLElement; body: HTMLElement }

const parsed = (key: string): unknown => { try { return JSON.parse(loadLocal(key) ?? "null"); } catch { return null; } };
/** A floating panel's size before it has been given one. */
const floatSize = (spec: PanelSpec) => ({ w: spec.size?.w ?? 300, h: spec.size?.h ?? 340 });
/** Past the window's edge: where a drag becomes a window of its own. */
const pastEdge = (ev: PointerEvent) => ev.clientX <= 0 || ev.clientY <= 0 || ev.clientX >= innerWidth - 1 || ev.clientY >= innerHeight - 1;

/** Make `parent`'s children exactly `kids`, in order, moving only what is out of place. An
 *  element re-inserted where it already is would still be detached and attached again —
 *  which reloads every iframe inside it, and the frame is full of them. */
function reconcile(parent: HTMLElement, kids: HTMLElement[]) {
  kids.forEach((k, i) => { if (parent.children[i] !== k) parent.insertBefore(k, parent.children[i] ?? null); });
  while (parent.children.length > kids.length) parent.lastElementChild!.remove();
}

class PanelHost {
  private specs = new Map<string, PanelSpec>();
  private frames = new Map<string, Frame>();
  private floating: Record<Mode, Floating> = {
    power: loadLocalJSON<Floating>(KEY + "power", { open: [], geom: {} }),
    den: loadLocalJSON<Floating>(KEY + "den", { open: [], geom: {} }),
  };
  /** Power's dock: the stored one, read once the panels are registered (`layoutDock`). */
  private tree: Node = defaultTree({ side: [], right: [], bottom: [] });
  /** Whether power's panels can be moved: dragged to another group or place, floated, or
   *  closed. Off, the dock holds still — tabs still switch, seams still resize — so a drag
   *  that started on a tab by accident never rearranges anything. Off by default. */
  private movable = loadLocal(MOVABLE_KEY) === "1";

  get canMove(): boolean {
    return this.movable;
  }

  setMovable(on: boolean): void {
    this.movable = on;
    this.keep(MOVABLE_KEY, on ? "1" : "0");
    this.apply();
  }

  /** The group each panel was last closed out of: where its letter puts it back. */
  private home: Record<string, string> = loadLocalJSON<Record<string, string>>(KEY + "home", {});
  private nodeEls = new Map<string, HTMLElement>();
  private groupEls = new Map<string, GroupEls>();
  private tabEls = new Map<string, HTMLElement>();
  private grips = new Map<string, HTMLElement>();
  /** Panels in windows of their own, and when each went out (for the heartbeat's grace,
   *  `viewsync.ts`). While one is out it is parked here. */
  readonly out = new Map<string, number>();
  /** A window opened from this page, by panel: a browser's handle, polled for closing. */
  private windows = new Map<string, Window>();
  private layer = () => document.getElementById("floating")!;
  private dockEl = () => document.getElementById("dock")!;
  /** In a window of its own (`?panel=`), the page is that panel and nothing else. */
  solo: string | null = null;

  /** The mode remembered, before anything is drawn. */
  initialMode(): Mode {
    return loadLocal(MODE_KEY) === "den" ? "den" : "power";
  }

  register(spec: PanelSpec): void {
    this.specs.set(spec.id, spec);
  }

  get all(): PanelSpec[] {
    return [...this.specs.values()];
  }

  isFloating(id: string): boolean {
    return this.floating[store.state.mode].open.includes(id) && !this.out.has(id);
  }

  /** In power's dock, whether or not its tab is the one showing. */
  private isDocked(id: string): boolean {
    return panelsIn(this.tree).includes(id);
  }

  /** On screen in this mode: its group's tab in front (power), or floating. */
  isVisible(id: string): boolean {
    if (this.out.has(id)) return false;
    if (this.isFloating(id)) return true;
    return store.state.mode === "power" && groupOf(this.tree, id)?.active === id;
  }

  setMode(mode: Mode): void {
    if (mode === store.state.mode) return;
    this.keep(MODE_KEY, mode);
    // The page says which mode it is in before anything is told: what follows the mode
    // (the frame's padding, den's viewfinder) measures a page already laid out for it.
    this.markApp(mode);
    store.set({ mode });
    this.apply();
  }

  private markApp(mode: Mode) {
    const app = document.getElementById("app")!;
    app.classList.toggle("den", mode === "den");
    app.classList.toggle("power", mode === "power");
    app.classList.toggle("dock-locked", !this.movable);
  }

  /**
   * What a panel's letter does. In den it summons the panel or sends it away. In power it
   * brings it to the front: its tab, if it is docked; back into the dock where it was, if it
   * was closed; floating, for the agent.
   */
  toggle(id: string): void {
    const spec = this.specs.get(id);
    if (!spec) return;
    // Out in a window of its own: the letter calls it back. Focusing that window instead
    // would do nothing at all if it had gone without saying so.
    if (this.out.has(id)) { this.recall(id); return; }
    if (store.state.mode === "den") { this.setFloating(id, !this.isFloating(id)); return; }
    this.show(id);
    this.flash(id);
  }

  /** Make sure `id` is up, without sending it away when it already is — called back from a
   *  window of its own, too: what asks for a panel wants it where it is looking. */
  show(id: string): void {
    if (!this.specs.has(id)) return;
    if (this.out.has(id)) { this.recall(id); return; }
    if (store.state.mode === "den") { if (!this.isFloating(id)) this.setFloating(id, true); return; }
    if (this.isFloating(id)) return;
    const g = groupOf(this.tree, id);
    if (g) { if (g.active !== id) { g.active = id; this.commit(); } return; }
    this.dockHome(id);
  }

  /** Summon `id` floating, or send it away. */
  setFloating(id: string, open: boolean): void {
    const l = this.floating[store.state.mode];
    l.open = l.open.filter((x) => x !== id);
    if (open) l.open.push(id);
    // In power a panel is in the dock or floating, never both.
    if (open && store.state.mode === "power") this.tree = remove(this.tree, id);
    this.commit();
  }

  /** Back into the dock: the group it was closed out of, or the place its kind lives. */
  private dockHome(id: string) {
    const spec = this.specs.get(id)!;
    const back = this.home[id] ? find(this.tree, this.home[id]) : undefined;
    // Else beside a panel of its kind: one that lives in the same place.
    const near = spec.dock ? this.all.filter((s) => s.dock === spec.dock).map((s) => groupOf(this.tree, s.id)).find((g) => g) : undefined;
    const group = back?.kind === "group" ? back : near;
    if (group) {
      group.tabs.push(id);
      group.active = id;
    } else if (spec.dock) {
      const target: Target = spec.dock === "bottom" ? { root: "bottom" } : { node: "viewer", zone: spec.dock === "side" ? "left" : "right" };
      this.tree = insert(this.tree, id, target, this.rect(spec.dock === "bottom" ? this.dockEl() : document.getElementById("center")!), spec.size);
    } else {
      this.setFloating(id, true);
      return;
    }
    this.commit();
  }

  /** Close a docked panel: out of the dock, remembered by the group it left. */
  close(id: string): void {
    const g = groupOf(this.tree, id);
    if (g) this.home[id] = g.id;
    this.tree = remove(this.tree, id);
    this.commit();
  }

  /** The dock as it starts: each panel with a home in the place it lives, in the order they
   *  were registered. */
  private firstTree(): Node {
    const homes: Homes = { side: [], right: [], bottom: [] };
    for (const s of this.all) if (s.dock) homes[s.dock].push(s.id);
    return defaultTree(homes);
  }

  /** Power's dock as it first was. */
  resetDock(): void {
    this.tree = this.firstTree();
    this.floating.power.open = [];
    this.commit();
  }

  private flash(id: string) {
    const frame = this.frames.get(id)?.root;
    const group = groupOf(this.tree, id);
    const box = this.isFloating(id) ? frame : group ? this.groupEls.get(group.id)?.root : null;
    box?.classList.add("flash");
    setTimeout(() => box?.classList.remove("flash"), 400);
  }

  /** Keep a piece of the layout — never from a panel's own window, whose page has no dock
   *  and would write over the editor window's. */
  private keep(key: string, v: unknown) {
    if (!this.solo) saveLocal(key, typeof v === "string" ? v : JSON.stringify(v));
  }
  private save() {
    this.keep(DOCK_KEY, this.tree);
    this.keep(KEY + "power", this.floating.power);
    this.keep(KEY + "den", this.floating.den);
    this.keep(KEY + "home", this.home);
  }
  /** Keep the layout as it now is, and put it on screen. */
  private commit() {
    this.save();
    this.apply();
  }

  private rect(e: HTMLElement): { w: number; h: number } {
    const r = e.getBoundingClientRect();
    return { w: r.width || innerWidth / 2, h: r.height || innerHeight / 2 };
  }

  /**
   * Draw the dock's containers — splits, groups and the frame — without filling them. Done
   * once before the preview mounts, so the frame is in its place before it has an overlay
   * iframe to lose by moving.
   */
  layoutDock(): void {
    if (this.solo) return;
    this.tree = sanitize(parsed(DOCK_KEY), new Set(this.specs.keys()), this.firstTree());
    this.drawDock(new Set());
  }

  /** The tree into the dock. Its root fills the dock, whatever it was before it was root. */
  private drawDock(away: Set<string>) {
    const root = this.nodeEl(this.tree, away);
    root.style.flex = "";
    reconcile(this.dockEl(), [root]);
  }

  /** Put every panel where the mode says, parking the rest. Called on every change of mode,
   *  dock, summons or windows — it is cheap: nothing is rebuilt, elements only move, and an
   *  element already in its place is not touched. */
  apply(): void {
    if (this.solo) return;
    const mode = store.state.mode;
    this.markApp(mode);
    const parking = document.getElementById("parking")!;
    const l = this.floating[mode];
    const floating = l.open.filter((id) => this.specs.has(id) && !this.out.has(id) && (mode === "den" || !this.isDocked(id)));
    // In den the dock is hidden under the frame; a panel floating there is not in it.
    const away = new Set([...floating, ...this.out.keys()]);
    const docked = panelsIn(this.tree).filter((id) => this.specs.has(id) && !away.has(id));

    for (const [id, f] of this.frames) {
      if (!docked.includes(id) && !floating.includes(id) && f.root.parentElement !== parking) parking.appendChild(f.root);
    }
    this.drawDock(away);
    const layer = this.layer();
    for (const id of floating) {
      const f = this.frame(id);
      f.root.className = "panel floating";
      f.root.hidden = false;
      if (f.root.parentElement !== layer) layer.appendChild(f.root);
      this.attach(f, f.tools, f.note);
    }
    this.reflow();
    document.dispatchEvent(new CustomEvent("kuma:panels"));
  }

  /** Floating panels into place, on their edges: after `apply`, and as the window resizes
   *  (once a frame — the dock is flex and follows by itself). */
  reflow(): void {
    const l = this.floating[store.state.mode];
    const floating = [...this.layer().querySelectorAll<HTMLElement>(":scope > .panel.floating")].map((e) => this.frames.get(e.dataset.panel!)!);
    const layer = this.layer().getBoundingClientRect();
    floating.forEach((f, i) => this.place(f, l.geom[f.spec.id] ?? this.defaultGeom(f.spec, i, layer), layer));
  }

  // ---- the dock, drawn ---------------------------------------------------------------

  /** A node's element, made once and kept by its id, with its children reconciled in. */
  private nodeEl(n: Node, away: Set<string>): HTMLElement {
    if (n.kind === "viewer") return document.getElementById("center")!;
    if (n.kind === "group") return this.groupEl(n, away);
    let e = this.nodeEls.get(n.id);
    if (!e) { e = el("div"); this.nodeEls.set(n.id, e); }
    e.className = `dock-split ${n.dir}`;
    const fill = fillOf(n);
    const kids: HTMLElement[] = [];
    n.children.forEach((c, i) => {
      if (i > 0) kids.push(this.grip(n, i));
      const ce = this.nodeEl(c, away);
      // A kept size is what it wants, not what it insists on: when the space runs short
      // (a window made smaller, a column that took one group too many) they all give.
      ce.style.flex = c === fill ? "1 1 0" : `${fill ? 0 : 1} 1 ${c.size ?? 240}px`;
      kids.push(ce);
    });
    reconcile(e, kids);
    return e;
  }

  private groupEl(g: Group, away: Set<string>): HTMLElement {
    let ge = this.groupEls.get(g.id);
    if (!ge) {
      const root = el("div", "dock-group");
      root.dataset.group = g.id;
      const head = el("div", "dock-head");
      const tabs = el("div", "dock-tabs");
      const note = el("span", "dock-note");
      const tools = el("span", "dock-tools");
      const pop = button("⧉", () => { const a = this.activeOf(g.id); if (a) this.popOut(a); }, "Open this panel in a window of its own (or drag its tab past the window's edge)", "pbtn pop");
      const close = button("×", () => { const a = this.activeOf(g.id); if (a) this.close(a); }, "Close this panel — its letter brings it back here", "pbtn pclose");
      head.append(tabs, note, el("span", "spacer"), tools, pop, close);
      // The head's empty part drags the panel in front, as its tab does.
      head.addEventListener("pointerdown", (e) => {
        if (this.movable && (e.target === head || (e.target as HTMLElement).classList.contains("spacer"))) { const a = this.activeOf(g.id); if (a) this.dragTab(e, a); }
      });
      const body = el("div", "dock-body");
      root.append(head, body);
      ge = { root, tabs, note, tools, body };
      this.groupEls.set(g.id, ge);
    }
    const shown = g.tabs.filter((t) => this.specs.has(t) && !away.has(t));
    // Every panel of it away (in windows, or floating in den): the group keeps its place
    // in the dock but has nothing to show.
    ge.root.hidden = shown.length === 0;
    const active = shown.includes(g.active) ? g.active : shown[0];
    reconcile(ge.tabs, g.tabs.filter((t) => this.specs.has(t)).map((t) => this.tabEl(t, t === active, this.out.has(t))));
    const frames = shown.map((t) => {
      const f = this.frame(t);
      f.root.className = "panel docked";
      f.root.removeAttribute("style");
      f.root.hidden = t !== active;
      return f;
    });
    reconcile(ge.body, frames.map((f) => f.root));
    const a = frames.find((f) => f.spec.id === active);
    if (a) this.attach(a, ge.tools, ge.note);
    else { ge.tools.replaceChildren(); ge.note.replaceChildren(); }
    for (const f of frames) if (f !== a && f.spec.body.parentElement !== f.body) f.body.replaceChildren(f.spec.body);
    return ge.root;
  }

  private activeOf(group: string): string | undefined {
    const g = find(this.tree, group);
    return g?.kind === "group" ? g.active : undefined;
  }

  /** A tab: the panel's letter, and its title while it is the one showing. Click shows it,
   *  a drag moves it, a right-click offers the rest. */
  private tabEl(id: string, on: boolean, out: boolean): HTMLElement {
    let t = this.tabEls.get(id);
    const spec = this.specs.get(id)!;
    if (!t) {
      t = el("div", "dock-tab");
      t.dataset.panel = id;
      t.append(el("span", "ptile", spec.letter), el("span", "ptitle", spec.title));
      t.addEventListener("pointerdown", (e) => { if (e.button === 0) this.dragTab(e, id); });
      t.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        const moving: MenuItem[] = this.movable ? [
          { label: "Float", run: () => this.setFloating(id, true), title: "Over the dock, where you drop it; drag it onto a group to dock it again" },
          { label: "Window of its own", run: () => this.popOut(id) },
          "separator",
          { label: "Close", run: () => this.close(id), title: "Its letter brings it back here" },
          { label: "Reset the dock", run: () => this.resetDock(), title: "Every panel back where it started" },
          { label: "Lock the panels", run: () => this.setMovable(false), title: "Hold them where they are" },
        ] : [
          { label: "Window of its own", run: () => this.popOut(id) },
          "separator",
          { label: "Let the panels move", run: () => this.setMovable(true), title: "Drag a tab to another group or beside one, float it, close it" },
        ];
        showMenu([{ heading: spec.title }, ...moving], e.clientX, e.clientY);
      });
    }
    t.classList.toggle("on", on);
    t.classList.toggle("out", out);
    const named = spec.key ? `${spec.title} (${spec.letter.toUpperCase()})` : spec.title;
    t.title = out ? `${spec.title} — in a window of its own; ${spec.key ? `its letter (${spec.letter.toUpperCase()}) calls it back` : "click to call it back"}`
      : this.movable ? `${named} — drag to move it: onto a group's middle to join it, onto an edge to split`
      : named;
    return t;
  }

  /** The seam before child `i` of a split: drag it to share the space differently. */
  private grip(s: Split, i: number): HTMLElement {
    const key = `${s.id}:${s.children[i].id}`;
    let g = this.grips.get(key);
    if (!g) {
      g = el("div");
      this.grips.set(key, g);
      g.addEventListener("pointerdown", (e) => {
        if (e.button !== 0) return;
        const split = find(this.tree, s.id) as Split | undefined;
        const b = split?.children.find((c) => `${s.id}:${c.id}` === key);
        if (!split || !b) return;
        const a = split.children[split.children.indexOf(b) - 1];
        const fill = fillOf(split);
        const grow = fill ? 0 : 1;
        const row = split.dir === "row";
        const size = (n: Node) => { const r = this.elOf(n).getBoundingClientRect(); return row ? r.width : r.height; };
        const [a0, b0] = [size(a), size(b)];
        g!.classList.add("on");
        dragPointer(e, {
          axes: "xy",
          move: (dx, dy) => {
            const d = row ? dx : dy;
            const clamp = (v: number) => Math.max(120, Math.round(v));
            if (a !== fill) a.size = clamp(a0 + d);
            if (b !== fill) b.size = clamp(b0 - d);
            for (const n of [a, b]) if (n !== fill) this.elOf(n).style.flex = `${grow} 1 ${n.size}px`;
          },
          end: (moved) => { g!.classList.remove("on"); if (moved) this.save(); },
        });
      });
    }
    g.className = `dock-grip ${s.dir}`;
    return g;
  }

  private elOf(n: Node): HTMLElement {
    return n.kind === "viewer" ? document.getElementById("center")! : n.kind === "group" ? this.groupEls.get(n.id)!.root : this.nodeEls.get(n.id)!;
  }

  // ---- moving panels by dragging -------------------------------------------------------

  /** Where a drop at (x, y) would put a panel in the dock: a group's middle or edge, the
   *  frame's edge, or the dock's own edge. Null off the dock. */
  private targetAt(x: number, y: number): { target: Target; box: DOMRect } | null {
    if (store.state.mode !== "power" || !this.movable) return null;
    const dock = this.dockEl().getBoundingClientRect();
    if (x < dock.left || x > dock.right || y < dock.top || y > dock.bottom) return null;
    const edge = (["left", "right", "top", "bottom"] as const).find((z) =>
      z === "left" ? x - dock.left < ROOT_EDGE : z === "right" ? dock.right - x < ROOT_EDGE : z === "top" ? y - dock.top < ROOT_EDGE : dock.bottom - y < ROOT_EDGE);
    if (edge) return { target: { root: edge }, box: half(dock, edge, 0.25) };
    const hit = document.elementsFromPoint(x, y).map((e) => (e as HTMLElement).closest<HTMLElement>(".dock-group, #center")).find((e) => e && this.dockEl().contains(e));
    if (!hit) return null;
    const box = hit.getBoundingClientRect();
    const rx = (x - box.left) / box.width, ry = (y - box.top) / box.height;
    const viewer = hit.id === "center";
    const near = [["left", rx], ["right", 1 - rx], ["top", ry], ["bottom", 1 - ry]] as const;
    const [zone, dist] = near.reduce((m, z) => (z[1] < m[1] ? z : m));
    // The middle of a group joins it; the frame has no middle to join, so its nearest edge.
    if (!viewer && dist > 0.25) return { target: { node: hit.dataset.group!, zone: "center" }, box };
    return { target: { node: viewer ? "viewer" : hit.dataset.group!, zone }, box: half(box, zone, 0.5) };
  }

  /**
   * Follow a drag that can end in the dock or past the window's edge: where it would land is
   * shown as it moves. `finish` clears that, and says whether the drop docked the panel or
   * gave it a window — otherwise it is the caller's to place.
   */
  private dropTracker() {
    let at: ReturnType<PanelHost["targetAt"]> = null;
    let outside = false;
    return {
      move: (ev: PointerEvent) => {
        outside = pastEdge(ev);
        // Under the pointer, not the panel's box: that is what is asked where it lands.
        at = this.targetAt(ev.clientX, ev.clientY);
        this.dropHint(at);
      },
      finish: (id: string, moved: boolean): boolean => {
        this.dropHint(null);
        if (!moved) return false;
        if (outside) { this.popOut(id); return true; }
        if (at) { this.dock(id, at.target); return true; }
        return false;
      },
    };
  }

  /** Show where a drop would land. */
  private dropHint(at: { box: DOMRect } | null) {
    let hint = document.getElementById("dock-drop");
    if (!at) { hint?.remove(); return; }
    if (!hint) { hint = el("div"); hint.id = "dock-drop"; document.body.appendChild(hint); }
    Object.assign(hint.style, { left: `${at.box.left}px`, top: `${at.box.top}px`, width: `${at.box.width}px`, height: `${at.box.height}px` });
  }

  /** Drag a docked panel by its tab: onto the dock to move it, off it to float it, past the
   *  window's edge for a window of its own. A press that does not move shows it. */
  private dragTab(e: PointerEvent, id: string) {
    // Locked: a press on a tab only shows it (or calls it back from its window).
    if (!this.movable) { this.show(id); return; }
    const spec = this.specs.get(id)!;
    const drop = this.dropTracker();
    let ghost: HTMLElement | null = null;
    let pointer = { x: e.clientX, y: e.clientY };
    dragPointer(e, {
      axes: "xy",
      move: (_dx, _dy, ev) => {
        pointer = { x: ev.clientX, y: ev.clientY };
        if (!ghost) {
          ghost = el("div", "dock-ghost");
          ghost.append(el("span", "ptile", spec.letter), el("span", "", spec.title));
          document.body.appendChild(ghost);
        }
        Object.assign(ghost.style, { left: `${ev.clientX + 12}px`, top: `${ev.clientY + 10}px` });
        drop.move(ev);
      },
      end: (moved) => {
        ghost?.remove();
        if (drop.finish(id, moved)) return;
        if (!moved) { this.show(id); return; }
        // Off the dock (over the top bar, the rail): it floats where it was let go.
        const layer = this.layer().getBoundingClientRect();
        this.floating.power.geom[id] = { x: pointer.x - layer.left - 40, y: pointer.y - layer.top - 10, ...floatSize(spec) };
        this.setFloating(id, true);
      },
    });
  }

  /** Put `id` at `target` in the dock, wherever it was before (a group, or floating). */
  private dock(id: string, target: Target): void {
    const from = groupOf(this.tree, id);
    // Onto its own group, alone in it: nowhere new to go.
    if (from && "node" in target && target.node === from.id && from.tabs.length === 1) return;
    if (from && "node" in target && target.node === from.id && target.zone === "center") { from.active = id; this.commit(); return; }
    const spec = this.specs.get(id)!;
    const node = "node" in target ? find(this.tree, target.node) : this.tree;
    const measured = node ? this.rect(this.elOf(node)) : this.rect(this.dockEl());
    const next = insert(structuredClone(this.tree), id, target, measured, spec.size);
    // A drop that would lose the panel (its target went with it) changes nothing.
    if (!panelsIn(next).includes(id) || !hasViewer(next)) return;
    this.tree = next;
    this.floating.power.open = this.floating.power.open.filter((x) => x !== id);
    this.commit();
  }

  /** The body, the tools and the note go in the container on screen; `show` lets a panel
   *  set itself up for being seen. */
  private attach(f: Frame, tools: HTMLElement, note: HTMLElement) {
    if (f.spec.body.parentElement !== f.body) f.body.replaceChildren(f.spec.body);
    // A panel with no tools (or note) clears the last one's out of a shared head.
    if (!f.spec.tools) tools.replaceChildren();
    else if (f.spec.tools.parentElement !== tools) tools.replaceChildren(f.spec.tools);
    if (!f.spec.note) note.replaceChildren();
    else if (f.spec.note.parentElement !== note) note.replaceChildren(f.spec.note);
    f.spec.show?.();
  }

  private defaultGeom(spec: PanelSpec, i: number, layer: DOMRect): Geom {
    const { w } = floatSize(spec);
    const h = Math.min(floatSize(spec).h, layer.height - 40);
    // Down the left then the right, so two summoned panels do not land on each other.
    if (spec.dock === "bottom") return { x: 30, y: Math.max(20, layer.height - h - 20), w: Math.min(w, layer.width - 60), h };
    const right = i % 2 === 1;
    // Below den's HUD (and power's top bar), which are what tell you where you are.
    return { x: right ? layer.width - w - 28 : 28, y: 68 + Math.floor(i / 2) * 40, w, h };
  }

  /** Put a floating panel at `g` in the floating layer (`layer`, measured by the caller once
   *  for every panel and every move rather than once each). */
  private place(f: Frame, g: Geom, layer: DOMRect) {
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
    const note = el("span", "pnote-slot");
    const tools = el("span", "ptools");
    const pop = button("⧉", () => this.popOut(id), "Open in a window of its own (or drag it past the window's edge)", "pbtn pop");
    const close = button("×", () => this.solo ? this.leave() : this.setFloating(id, false), "Close", "pbtn");
    head.append(tile, title, note, el("span", "spacer"), tools, pop, close);
    if (spec.note) spec.note.classList.add("pnote");
    const body = el("div", "panel-body");
    root.append(head, body);
    f = { root, body, spec, tools, note, pop };
    this.frames.set(id, f);
    this.dragging(f, head);
    this.resizers(f);
    return f;
  }

  /** Drag a floating panel by its title bar. Over power's dock it docks where the hint
   *  says; within `SNAP` of the window's edge it sticks to it; past the edge it becomes a
   *  window of its own. */
  private dragging(f: Frame, head: HTMLElement) {
    head.addEventListener("pointerdown", (e) => {
      if (e.button !== 0 || !f.root.classList.contains("floating") || (e.target as HTMLElement).closest("button, input, select")) return;
      const { layer, g } = this.geomOf(f);
      const x0 = g.x, y0 = g.y;
      const drop = this.dropTracker();
      let hint: HTMLElement | null = null;
      f.root.classList.add("dragging");
      dragPointer(e, {
        axes: "xy",
        move: (dx, dy, ev) => {
          g.x = x0 + dx;
          g.y = y0 + dy;
          g.edge = undefined;
          this.place(f, g, layer);
          drop.move(ev);
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
          if (drop.finish(f.spec.id, moved) || !moved) return;
          // Snapped: flush with the edge it was dropped near, and kept there.
          if (g.x < SNAP) g.edge = "left";
          else if (layer.width - (g.x + g.w) < SNAP) g.edge = "right";
          else if (layer.height - (g.y + g.h) < SNAP && f.spec.dock === "bottom") g.edge = "bottom";
          this.floating[store.state.mode].geom[f.spec.id] = g;
          this.place(f, g, layer);
          this.save();
        },
      });
    });
  }

  /**
   * Resize a floating panel from any of its edges or corners — a strip along each side and
   * a square at each corner, each with its own cursor, and a grip drawn in the bottom-right
   * corner so there is something to see. It stays inside the window, and no smaller than
   * a panel can be used at. A panel held against an edge stays held while its other side
   * moves, and lets go when the side against the edge is the one dragged.
   */
  private resizers(f: Frame) {
    for (const dir of ["n", "s", "e", "w", "ne", "nw", "se", "sw"] as const) {
      const h = el("div", `prs ${dir}`);
      f.root.appendChild(h);
      h.addEventListener("pointerdown", (e) => {
        if (e.button !== 0 || !f.root.classList.contains("floating")) return;
        e.stopPropagation();
        const { layer, g } = this.geomOf(f);
        const x0 = g.x, y0 = g.y, w0 = g.w, h0 = g.h;
        const saved = this.floating[store.state.mode].geom[f.spec.id];
        // A panel held against the bottom is drawn full width: its own width is the kept one.
        g.w = saved?.w ?? w0;
        g.edge = saved?.edge;
        // The side held against an edge is the one that lets go when it is dragged.
        if ((g.edge === "left" && dir.includes("w")) || (g.edge === "right" && dir.includes("e")) || (g.edge === "bottom" && dir.includes("s"))) g.edge = undefined;
        const clamp = (v: number, lo: number, hi: number) => Math.round(Math.max(lo, Math.min(hi, v)));
        f.root.classList.add("resizing");
        dragPointer(e, {
          axes: "xy",
          move: (dx, dy) => {
            if (dir.includes("e")) g.w = clamp(w0 + dx, MIN_W, layer.width - x0);
            if (dir.includes("w")) { g.w = clamp(w0 - dx, MIN_W, x0 + w0); g.x = x0 + w0 - g.w; }
            if (dir.includes("s")) g.h = clamp(h0 + dy, MIN_H, layer.height - y0);
            if (dir.includes("n")) { g.h = clamp(h0 - dy, MIN_H, y0 + h0); g.y = y0 + h0 - g.h; }
            this.place(f, g, layer);
          },
          end: (moved) => {
            f.root.classList.remove("resizing");
            if (!moved) return;
            this.floating[store.state.mode].geom[f.spec.id] = g;
            this.save();
          },
        });
      });
    }
  }

  /** Where a floating panel is now, in the floating layer, and the layer: what a drag or a
   *  resize starts from. */
  private geomOf(f: Frame): { layer: DOMRect; g: Geom } {
    const layer = this.layer().getBoundingClientRect();
    const r = f.root.getBoundingClientRect();
    return { layer, g: { x: r.left - layer.left, y: r.top - layer.top, w: r.width, h: r.height } };
  }


  /**
   * A panel in a window of its own — a second screen's worth of timeline, say. A browser
   * opens a popup on `?panel=<id>`; the desktop shell opens a frameless window through its
   * bridge. Either way it is this same page, showing that panel alone, kept in step with
   * this one by `viewsync.ts`. While it is out, this window keeps its place (its tab stays,
   * greyed) and parks it; closing that window brings it back there.
   */
  popOut(id: string): void {
    if (this.solo) return;
    const spec = this.specs.get(id);
    if (!spec || this.out.has(id)) return;
    const shown = this.frames.get(id)?.root;
    const r = shown?.isConnected && !shown.hidden ? shown.getBoundingClientRect() : null;
    // As big as it is here, and no smaller than a window worth having; the shell keeps it
    // within the screen.
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
    this.out.set(id, Date.now());
    this.apply();
  }

  /** A panel's window closed (or said so): it comes back where it was — its tab in power's
   *  dock, or summoned in den, since putting it back is asking for it. */
  returned(id: string): void {
    this.windows.delete(id);
    if (!this.out.delete(id)) return;
    this.show(id);
    this.apply();
  }

  /** Bring a panel back from its window, closing the window if it is still there. */
  private recall(id: string): void {
    this.onRecall(id);
    this.windows.get(id)?.close();
    this.returned(id);
  }

  /** How the editor window asks a panel's window to close (`viewsync.ts` sets it). */
  onRecall: (id: string) => void = () => {};
  /** How a panel's window leaves: told to the editor window first, then closed. */
  leave: () => void = () => window.close();

  /** Told that a panel is in a window (a window opened before this page reloaded). */
  claimed(id: string): void {
    if (!this.specs.has(id)) return;
    const was = this.out.has(id);
    this.out.set(id, Date.now());
    if (!was) this.apply();
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
    f.pop.textContent = "↙";
    f.pop.title = "Put it back in the editor window";
    f.pop.onclick = () => this.leave();
    this.layer().appendChild(f.root);
    this.attach(f, f.tools, f.note);
    document.title = `${spec.title} — kuma`;
    return true;
  }
}

/** The `edge` part of `r`: `share` of it along that edge (the middle, for "center"). */
function half(r: DOMRect, edge: string, share: number): DOMRect {
  const w = r.width * share, h = r.height * share;
  switch (edge) {
    case "left": return new DOMRect(r.left, r.top, w, r.height);
    case "right": return new DOMRect(r.right - w, r.top, w, r.height);
    case "top": return new DOMRect(r.left, r.top, r.width, h);
    case "bottom": return new DOMRect(r.left, r.bottom - h, r.width, h);
    default: return r;
  }
}

export const panels = new PanelHost();

export function panelCommands(): void {
  for (const s of panels.all) command(`panel.${s.id}`, `Panel: ${s.title}`, () => panels.toggle(s.id));
  command(Cmd.modeDen, "Den mode", () => panels.setMode("den"));
  command(Cmd.modePower, "Power mode", () => panels.setMode("power"));
  command(Cmd.toggleMode, "Switch den / power", () => panels.setMode(store.state.mode === "den" ? "power" : "den"));
  command(Cmd.resetDock, "Reset the dock", () => panels.resetDock());
  command(Cmd.movablePanels, "Movable panels: on / off", () => {
    panels.setMovable(!panels.canMove);
    toast(panels.canMove ? "Panels move: drag a tab to another group, beside one or off the dock" : "Panels locked where they are", "ok");
  });
  for (const s of panels.all) command(`panel.${s.id}.window`, `Window: ${s.title}`, () => panels.popOut(s.id));
  for (const s of panels.all) command(`panel.${s.id}.float`, `Float: ${s.title}`, () => panels.setFloating(s.id, true));
  // The window may resize under floating panels: keep them on screen and on their edges,
  // once a frame however fast the window is dragged.
  let pending = 0;
  addEventListener("resize", () => { if (!pending) pending = requestAnimationFrame(() => { pending = 0; panels.reflow(); }); });
}
