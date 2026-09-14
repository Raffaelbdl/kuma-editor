// Timeline: tracks, clips (move/trim), transitions, playhead, zoom.

import { button, el } from "./dom";
import { store, toast } from "./store";
import type { Clip, PatchOp, Project, Track, TrackKind } from "./types";
import { clipKind, freeId, isAsset, isOverlay } from "./types";

const HEAD = 120;

export class Timeline {
  private root: HTMLDivElement;
  private playhead: HTMLDivElement | null = null;
  private drag: null | { kind: "move" | "trim-l" | "trim-r"; clip: Clip; ti: number; startX: number; orig: { start: number; end: number; in: number }; ghost: HTMLDivElement; toTrack: number } = null;

  constructor(private host: HTMLElement) {
    this.root = el("div", "tl");
    host.appendChild(this.root);
    // Only the project and the zoom rebuild the DOM; selection and playhead are cheap updates.
    store.select((s) => [s.project, s.zoom], () => this.render());
    store.select((s) => [s.selection], (s) => {
      for (const c of this.root.querySelectorAll<HTMLElement>(".clip:not(.ghost)")) c.classList.toggle("selected", c.dataset.id === s.selection);
    });
    store.select((s) => [s.playhead], (s) => this.movePlayhead(s.playhead));
    host.addEventListener("wheel", (e) => {
      if (e.ctrlKey || e.metaKey) {
        e.preventDefault();
        const z = Math.min(600, Math.max(8, store.state.zoom * (e.deltaY < 0 ? 1.15 : 1 / 1.15)));
        store.set({ zoom: z });
      }
    }, { passive: false });
    host.addEventListener("dragover", (e) => { e.preventDefault(); });
    host.addEventListener("drop", (e) => this.onDrop(e));
  }

  private x(t: number) { return HEAD + t * store.state.zoom; }
  private t(x: number) { return (x - HEAD) / store.state.zoom; }

  private render() {
    const s = store.state;
    const p = s.project;
    if (!p) return;
    // A re-render (agent edit, undo) replaces the ghost's parent: drop the drag rather
    // than commit against a clip that may have moved.
    this.drag = null;
    const width = Math.max(this.host.clientWidth, this.x(store.duration + 10));
    this.root.style.width = `${width}px`;
    this.root.innerHTML = "";

    // Ruler
    const ruler = el("div", "tl-ruler");
    const step = niceStep(s.zoom);
    for (let i = 0; i * step <= store.duration + 10; i++) {
      const t = i * step; // by index, so float drift never turns "3s" into "3.0"
      const tick = el("div", "tick", Number.isInteger(t) ? `${t}s` : t.toFixed(1));
      tick.style.left = `${this.x(t)}px`;
      ruler.appendChild(tick);
    }
    ruler.addEventListener("pointerdown", (e) => this.scrub(e));
    this.root.appendChild(ruler);

    // Tracks (top track of the composite is drawn first, i.e. reverse of array order)
    const tracks = [...p.tracks].map((t, i) => ({ t, i })).reverse();
    for (const { t, i } of tracks) this.root.appendChild(this.trackEl(p, t, i));
    if (!p.tracks.length) {
      const hint = el("div", "muted", "No tracks yet — use “Add at playhead” in the library; the right track is created for you.");
      hint.style.padding = "16px";
      this.root.appendChild(hint);
    }

    // Markers and playhead
    for (const m of p.markers) {
      const mk = el("div", "tl-marker");
      mk.style.left = `${this.x(m.t)}px`;
      mk.title = m.label;
      this.root.appendChild(mk);
    }
    this.playhead = el("div", "tl-playhead");
    this.root.appendChild(this.playhead);
    this.movePlayhead(s.playhead);
  }

  private movePlayhead(t: number) {
    if (this.playhead) this.playhead.style.left = `${this.x(t)}px`;
  }

  private trackEl(p: Project, t: Track, ti: number): HTMLDivElement {
    const row = el("div", `tl-track ${t.kind}`);
    row.dataset.ti = String(ti);
    const head = el("div", "tl-head");
    const label = el("div");
    label.append(el("div", "kind", t.kind), el("div", "", t.name || t.id));
    const toggles = el("div", "toggles");
    const toggle = (text: string, on: boolean, field: "muted" | "hidden") => {
      const b = button(text, () => store.patch([{ op: "replace", path: `/tracks/${ti}/${field}`, value: !on }], `${field} ${t.id}`), field === "muted" ? "Mute" : "Hide");
      b.className = on ? "on" : "";
      return b;
    };
    if (t.kind !== "overlay") toggles.appendChild(toggle("M", t.muted, "muted"));
    if (t.kind !== "audio") toggles.appendChild(toggle("H", t.hidden, "hidden"));
    if (!t.clips.length) toggles.appendChild(button("×", () => store.patch([{ op: "remove", path: `/tracks/${ti}` }], `remove track ${t.id}`), "Remove this empty track"));
    head.append(label, toggles);
    row.appendChild(head);

    const lane = el("div", "tl-lane");
    lane.addEventListener("pointerdown", (e) => { if (e.target === lane) { store.set({ selection: null }); this.scrub(e); } });
    for (const c of t.clips) lane.appendChild(this.clipEl(p, t, ti, c));
    for (const x of t.transitions) {
      const b = t.clips.find((c) => c.id === x.to);
      if (!b || !t.clips.some((c) => c.id === x.from)) continue;
      const xf = el("div", "tl-xfade");
      xf.style.left = `${this.x(b.start - x.duration / 2) - HEAD}px`;
      xf.style.width = `${x.duration * store.state.zoom}px`;
      xf.title = `${x.kind} ${x.duration}s`;
      lane.appendChild(xf);
    }
    row.appendChild(lane);
    return row;
  }

  private clipEl(p: Project, t: Track, ti: number, c: Clip): HTMLDivElement {
    const w = Math.max(4, (c.end - c.start) * store.state.zoom);
    const box = el("div", `clip ${clipKind(p, c, t)}${store.state.selection === c.id ? " selected" : ""}${w < 24 ? " tiny" : ""}`);
    box.dataset.id = c.id;
    box.style.left = `${this.x(c.start) - HEAD}px`;
    box.style.width = `${w}px`;
    const name = isOverlay(c) ? c.src.overlay : isAsset(c) ? c.src.asset : "colour";
    const sub = isOverlay(c) ? Object.values(c.src.vars ?? {}).find((v) => typeof v === "string") ?? "" : c.speed !== 1 ? `${c.speed}×` : "";
    box.title = `${c.id}  ${c.start.toFixed(2)}–${c.end.toFixed(2)}s`;
    const hl = el("div", "handle l");
    const hr = el("div", "handle r");
    box.append(el("div", "", c.label || name), el("div", "sub", String(sub)), hl, hr);
    box.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      const kind = e.target === hl ? "trim-l" : e.target === hr ? "trim-r" : "move";
      this.beginDrag(e, kind, c, ti, box);
      store.set({ selection: c.id });
    });
    return box;
  }

  private scrub(e: PointerEvent) {
    const move = (ev: PointerEvent) => {
      const rect = this.root.getBoundingClientRect();
      store.seek(this.t(ev.clientX - rect.left));
    };
    move(e);
    const up = () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up); };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  private beginDrag(e: PointerEvent, kind: "move" | "trim-l" | "trim-r", clip: Clip, ti: number, el: HTMLDivElement) {
    const ghost = el.cloneNode(true) as HTMLDivElement;
    ghost.classList.add("ghost");
    el.parentElement!.appendChild(ghost);
    this.drag = { kind, clip, ti, startX: e.clientX, orig: { start: clip.start, end: clip.end, in: isAsset(clip) ? clip.src.in ?? 0 : 0 }, ghost, toTrack: ti };
    const move = (ev: PointerEvent) => this.onDragMove(ev);
    const up = () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up); this.endDrag(); };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  private snapT(t: number, self: Clip): number {
    const p = store.state.project!;
    const thresh = 8 / store.state.zoom;
    const cands = [0, store.state.playhead, ...p.tracks.flatMap((tr) => tr.clips.filter((c) => c.id !== self.id).flatMap((c) => [c.start, c.end]))];
    for (const c of cands) if (Math.abs(c - t) < thresh) return c;
    return store.snap(t);
  }

  private onDragMove(ev: PointerEvent) {
    const d = this.drag;
    if (!d) return;
    const dt = (ev.clientX - d.startX) / store.state.zoom;
    const dur = d.orig.end - d.orig.start;
    let start = d.orig.start, end = d.orig.end;
    if (d.kind === "move") {
      start = Math.max(0, this.snapT(d.orig.start + dt, d.clip));
      end = start + dur;
      // Allow moving between tracks of the same kind.
      const row = (ev.target as HTMLElement | null)?.closest?.(".tl-track") as HTMLElement | null;
      const target = row ? Number(row.dataset.ti) : d.ti;
      const p = store.state.project!;
      const same = p.tracks[target]?.kind === p.tracks[d.ti].kind;
      const to = same ? target : d.ti;
      if (to !== d.toTrack) {
        d.toTrack = to;
        this.root.querySelector(`[data-ti="${to}"] .tl-lane`)?.appendChild(d.ghost);
      }
    } else if (d.kind === "trim-l") {
      start = Math.min(end - 1 / store.fps, Math.max(0, this.snapT(d.orig.start + dt, d.clip)));
      // Cannot reveal material before the source's first frame (handles scale with speed).
      const speed = Math.abs(d.clip.speed) || 1;
      if (isAsset(d.clip) && d.orig.in + (start - d.orig.start) * speed < 0) start = store.snap(d.orig.start - d.orig.in / speed);
    } else {
      end = Math.max(start + 1 / store.fps, this.snapT(d.orig.end + dt, d.clip));
    }
    d.ghost.style.left = `${this.x(start) - HEAD}px`;
    d.ghost.style.width = `${Math.max(4, (end - start) * store.state.zoom)}px`;
    d.ghost.dataset.start = String(start);
    d.ghost.dataset.end = String(end);
  }

  private async endDrag() {
    const d = this.drag;
    this.drag = null;
    if (!d) return;
    const start = Number(d.ghost.dataset.start ?? d.orig.start);
    const end = Number(d.ghost.dataset.end ?? d.orig.end);
    d.ghost.remove();
    if (start === d.orig.start && end === d.orig.end && d.toTrack === d.ti) return;
    const path = store.clipPath(d.clip.id);
    if (!path) return; // the clip went away mid-drag (undo, agent edit)
    const ops: PatchOp[] = [];
    if (d.kind === "trim-l" && isAsset(d.clip)) {
      ops.push({ op: "replace", path: `${path}/src/in`, value: Math.max(0, d.orig.in + (start - d.orig.start) * Math.abs(d.clip.speed)) });
    }
    if (d.toTrack !== d.ti) {
      // Transitions are per track: any that involved this clip go with the cut.
      const xs = store.state.project!.tracks[d.ti].transitions;
      if (xs.some((x) => x.from === d.clip.id || x.to === d.clip.id)) {
        ops.push({ op: "replace", path: `/tracks/${d.ti}/transitions`, value: xs.filter((x) => x.from !== d.clip.id && x.to !== d.clip.id) });
      }
      const moved = { ...d.clip, start, end };
      ops.push({ op: "remove", path }, { op: "add", path: `/tracks/${d.toTrack}/clips/-`, value: moved });
    } else {
      ops.push({ op: "replace", path: `${path}/start`, value: start }, { op: "replace", path: `${path}/end`, value: end });
    }
    await store.patch(ops, `${d.kind} ${d.clip.id}`);
  }

  private async onDrop(e: DragEvent) {
    e.preventDefault();
    const data = e.dataTransfer?.getData("application/x-kuma");
    if (!data) return;
    const item = JSON.parse(data) as { template?: string; asset?: string };
    const rect = this.root.getBoundingClientRect();
    const t = store.snap(this.t(e.clientX - rect.left));
    const row = (e.target as HTMLElement).closest(".tl-track") as HTMLElement | null;
    const ti = row ? Number(row.dataset.ti) : -1;
    await addItem(item, t, ti);
  }
}

/** Add a template instance or an asset clip at time `t` on track `ti`, falling back to the
 *  first track of the right kind (created if none exists). If the slot is taken the clip
 *  is appended after the track's last clip instead. */
export async function addItem(item: { template?: string; asset?: string }, t: number, ti = -1) {
  const p = store.state.project!;
  let clip: Clip;
  let kind: TrackKind;
  const base = { speed: 1, volume: 1, muted: false, effects: [], label: "" };
  if (item.template) {
    const tpl = store.state.templates.find((x) => x.id === item.template);
    const d = tpl?.manifest.duration_default ?? 4;
    clip = { id: store.newId("o"), start: t, end: t + d, src: { overlay: item.template, vars: {} }, ...base };
    kind = "overlay";
  } else if (item.asset) {
    const a = p.assets[item.asset];
    const d = a?.meta?.duration ?? 5;
    clip = { id: store.newId("c"), start: t, end: t + d, src: { asset: item.asset, in: 0 }, ...base };
    kind = a?.kind === "audio" ? "audio" : "video";
  } else return;
  if (ti < 0 || p.tracks[ti]?.kind !== kind) ti = p.tracks.findIndex((tr) => tr.kind === kind);
  const ops: PatchOp[] = [];
  if (ti < 0) {
    // No track of that kind yet (e.g. a project created by hand): add one, then the clip.
    const { op, index } = newTrackOp(kind);
    ti = index;
    ops.push(op);
    toast(`Added a ${kind} track`, "ok");
  } else {
    // Avoid overlaps: push to the end of the track if the slot is taken.
    const track = p.tracks[ti];
    if (track.clips.some((c) => clip.start < c.end && clip.end > c.start)) {
      const endT = Math.max(0, ...track.clips.map((c) => c.end));
      clip.end = endT + (clip.end - clip.start);
      clip.start = endT;
    }
  }
  ops.push({ op: "add", path: `/tracks/${ti}/clips/-`, value: clip });
  const ok = await store.patch(ops, `add ${clip.id}`);
  if (ok) store.set({ selection: clip.id });
}

/** Build the patch op that inserts a new empty track of `kind`. Tracks composite in array
 *  order, so a video track goes below the first overlay track and everything else on top
 *  (audio order does not matter). */
function newTrackOp(kind: TrackKind): { op: PatchOp; index: number } {
  const p = store.state.project!;
  const id = freeId(kind[0], p.tracks.map((t) => t.id));
  const name = `${kind[0].toUpperCase()}${kind.slice(1)} ${id.slice(1)}`;
  const track: Track = { id, kind, name, muted: false, hidden: false, clips: [], transitions: [] };
  const firstOverlay = p.tracks.findIndex((t) => t.kind === "overlay");
  const index = kind === "video" && firstOverlay >= 0 ? firstOverlay : p.tracks.length;
  return { op: { op: "add", path: `/tracks/${index}`, value: track }, index };
}

/** Add an empty track from the UI. */
export async function addTrack(kind: TrackKind) {
  const { op } = newTrackOp(kind);
  await store.patch([op], `add ${kind} track`);
}

function niceStep(zoom: number): number {
  const target = 80 / zoom; // seconds per ~80px
  const steps = [0.1, 0.25, 0.5, 1, 2, 5, 10, 15, 30, 60];
  return steps.find((s) => s >= target) ?? 60;
}
