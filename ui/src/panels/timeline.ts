// Timeline: tracks, clips (move/trim), transitions, playhead, zoom.

import { button, dragPayload, el } from "../dom";
import { placeBinItem, addFromLibrary } from "../edits";
import { keysOf } from "../keyframes";
import { store } from "../store";
import type { Clip, PatchOp, Project, Track, TrackKind } from "../types";
import { binItem, clipKind, isOverlaySource, isSequenceSource, isTimedMedia, seqDuration, sequenceOf } from "../types";

const HEAD = 136;

export class Timeline {
  private root: HTMLDivElement;
  private playhead: HTMLDivElement | null = null;
  private drag: null | { kind: "move" | "trim-l" | "trim-r"; clip: Clip; ti: number; startX: number; orig: { start: number; end: number; in: number }; ghost: HTMLDivElement; toTrack: number } = null;

  constructor(private host: HTMLElement) {
    this.root = el("div", "tl");
    host.appendChild(this.root);
    // Only the project, the open sequence and the zoom rebuild the DOM; selection and
    // playhead are cheap updates.
    store.select((s) => [s.project, s.sequence, s.zoom], () => this.render());
    store.select((s) => [s.selection], () => {
      const sel = new Set(store.selectedClips);
      for (const c of this.root.querySelectorAll<HTMLElement>(".clip:not(.ghost)")) c.classList.toggle("selected", sel.has(c.dataset.id!));
    });
    store.select((s) => [s.playhead], (s) => this.movePlayhead(s.playhead));
    // Which track a paste lands on is a click away and must show, so it is toggled here
    // rather than rebuilt: the same treatment the selection gets, for the same reason.
    store.select((s) => [s.track], (s) => {
      for (const row of this.root.querySelectorAll<HTMLElement>(".tl-track")) row.classList.toggle("focus", row.dataset.tid === s.track);
    });
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
    const end = store.duration + 10;
    for (let i = 0; i * step <= end; i++) {
      const t = i * step; // by index, so float drift never turns "3s" into "3.0"
      const tick = el("div", "tick", Number.isInteger(t) ? `${t}s` : t.toFixed(1));
      tick.style.left = `${this.x(t)}px`;
      ruler.appendChild(tick);
    }
    ruler.addEventListener("pointerdown", (e) => this.scrub(e));
    this.root.appendChild(ruler);

    // Drawn overlays first, then video, then audio: the order they composite in, top
    // down, with audio — which composites in no order at all — out of the way at the
    // bottom. Within a kind the array order is reversed, because a later track
    // composites over an earlier one. Only the drawing is grouped; the document's own
    // order is what the compiler reads, and `data-ti` still names it.
    const tracks = store.tracks.map((t, i) => ({ t, i })).sort((a, b) => LANE_ORDER[a.t.kind] - LANE_ORDER[b.t.kind] || b.i - a.i);
    for (const { t, i } of tracks) this.root.appendChild(this.trackEl(p, t, i));
    if (!store.tracks.length) {
      const hint = el("div", "muted", "No tracks yet — use “Add at playhead” in the library; the right track is created for you.");
      hint.style.padding = "16px";
      this.root.appendChild(hint);
    }

    // Markers and playhead
    for (const m of sequenceOf(p, s.sequence)?.markers ?? []) {
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
    const row = el("div", `tl-track ${t.kind}${store.state.track === t.id ? " focus" : ""}`);
    row.dataset.ti = String(ti);
    row.dataset.tid = t.id;
    const head = el("div", "tl-head");
    head.title = `${t.name || t.id} — click a track to paste onto it (Ctrl/Cmd+V pastes at the playhead)`;
    // Anywhere on the head, buttons included: clicking a track is how a paste is aimed.
    head.addEventListener("pointerdown", () => store.focusTrack(t.id));
    const label = el("div", "label");
    label.append(el("div", "kind", t.kind), el("div", "tname", t.name || t.id));
    const toggles = el("div", "toggles");
    const toggle = (text: string, on: boolean, field: "muted" | "hidden") => {
      const b = button(text, () => store.patch([{ op: "replace", path: `${store.seqPath}/tracks/${ti}/${field}`, value: !on }], `${field} ${t.id}`), field === "muted" ? "Mute" : "Hide");
      b.className = on ? "on" : "";
      return b;
    };
    if (t.kind !== "overlay") toggles.appendChild(toggle("M", t.muted, "muted"));
    if (t.kind !== "audio") toggles.appendChild(toggle("H", t.hidden, "hidden"));
    if (!t.clips.length) toggles.appendChild(button("×", () => store.patch([{ op: "remove", path: `${store.seqPath}/tracks/${ti}` }], `remove track ${t.id}`), "Remove this empty track"));
    head.append(label, toggles);
    row.appendChild(head);

    const lane = el("div", "tl-lane");
    // An empty lane is the plainest way to say "this track, this moment": it aims the
    // paste and takes the playhead with it, which is both halves of where a paste goes.
    lane.addEventListener("pointerdown", (e) => { if (e.target === lane) { store.set({ selection: null, track: t.id }); this.scrub(e); } });
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
    const box = el("div", `clip ${clipKind(p, c, t)}${store.selectedClip === c.id ? " selected" : ""}${w < 24 ? " tiny" : ""}`);
    box.dataset.id = c.id;
    box.style.left = `${this.x(c.start) - HEAD}px`;
    box.style.width = `${w}px`;
    const item = binItem(p, c);
    const name = item ? item.name || c.bin : `? ${c.bin}`;
    // The template is worth showing only when the entry was not named after it.
    const template = item && isOverlaySource(item.source) && item.source.template !== name ? item.source.template : "";
    const seq = item && isSequenceSource(item.source) ? `sequence · ${seqDuration(p, c.bin).toFixed(1)}s` : "";
    const sub = c.speed !== 1 ? `${c.speed}×` : template || seq;
    box.title = `${c.id}  ${c.start.toFixed(2)}–${c.end.toFixed(2)}s`;
    const hl = el("div", "handle l");
    const hr = el("div", "handle r");
    box.append(el("div", "", c.label || name), el("div", "sub", String(sub)), hl, hr);
    // Keyframed effects are drawn along the bottom of the clip: an animated clip says so
    // at a glance, and the keys go past under the playhead while you scrub. They take no
    // pointer events — editing them is the inspector's, where the values are.
    for (const k of keysOf(c)) {
      const lane = el("div", `kf-lane ${k.type}`);
      for (const t of k.times) {
        const key = el("div", "kf");
        key.style.left = `${t * store.state.zoom}px`;
        lane.appendChild(key);
      }
      box.appendChild(lane);
    }
    box.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      // Clicking a clip is also clicking the track it is on, so a copy followed by a
      // paste with nothing else clicked lands back on the track it came from.
      store.focusTrack(t.id);
      // Ctrl/Cmd or Shift builds a selection of several clips, and does not drag: adding a
      // clip to a selection and moving one are different intentions, and a drag that
      // started on the modifier would move one clip out of a group that looks joined.
      if (e.ctrlKey || e.metaKey || e.shiftKey) {
        store.toggleClip(c.id);
        return;
      }
      const kind = e.target === hl ? "trim-l" : e.target === hr ? "trim-r" : "move";
      this.beginDrag(e, kind, c, ti, box);
      store.selectItem("clip", c.id);
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
    this.drag = { kind, clip, ti, startX: e.clientX, orig: { start: clip.start, end: clip.end, in: clip.in ?? 0 }, ghost, toTrack: ti };
    const move = (ev: PointerEvent) => this.onDragMove(ev);
    const up = () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up); this.endDrag(); };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  private snapT(t: number, self: Clip): number {
    const thresh = 8 / store.state.zoom;
    const cands = [0, store.state.playhead, ...store.tracks.flatMap((tr) => tr.clips.filter((c) => c.id !== self.id).flatMap((c) => [c.start, c.end]))];
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
      const same = store.tracks[target]?.kind === store.tracks[d.ti].kind;
      const to = same ? target : d.ti;
      if (to !== d.toTrack) {
        d.toTrack = to;
        this.root.querySelector(`[data-ti="${to}"] .tl-lane`)?.appendChild(d.ghost);
      }
    } else if (d.kind === "trim-l") {
      start = Math.min(end - 1 / store.fps, Math.max(0, this.snapT(d.orig.start + dt, d.clip)));
      // Cannot reveal material before the source's first frame (handles scale with speed).
      const speed = Math.abs(d.clip.speed) || 1;
      const timed = isTimedMedia(binItem(store.state.project!, d.clip));
      if (timed && d.orig.in + (start - d.orig.start) * speed < 0) start = store.snap(d.orig.start - d.orig.in / speed);
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
    if (d.kind === "trim-l" && isTimedMedia(binItem(store.state.project!, d.clip))) {
      ops.push({ op: "replace", path: `${path}/in`, value: Math.max(0, d.orig.in + (start - d.orig.start) * Math.abs(d.clip.speed)) });
    }
    if (d.toTrack !== d.ti) {
      // Transitions are per track: any that involved this clip go with the cut.
      const xs = store.tracks[d.ti].transitions;
      if (xs.some((x) => x.from === d.clip.id || x.to === d.clip.id)) {
        ops.push({ op: "replace", path: `${store.seqPath}/tracks/${d.ti}/transitions`, value: xs.filter((x) => x.from !== d.clip.id && x.to !== d.clip.id) });
      }
      const moved = { ...d.clip, start, end };
      ops.push({ op: "remove", path }, { op: "add", path: `${store.seqPath}/tracks/${d.toTrack}/clips/-`, value: moved });
    } else {
      ops.push({ op: "replace", path: `${path}/start`, value: start }, { op: "replace", path: `${path}/end`, value: end });
    }
    await store.patch(ops, `${d.kind} ${d.clip.id}`);
  }

  private async onDrop(e: DragEvent) {
    e.preventDefault();
    const payload = dragPayload(e);
    if (!payload) return;
    const rect = this.root.getBoundingClientRect();
    const t = store.snap(this.t(e.clientX - rect.left));
    const row = (e.target as HTMLElement).closest(".tl-track") as HTMLElement | null;
    const ti = row ? Number(row.dataset.ti) : -1;
    if (payload.bin) await placeBinItem(payload.bin, t, ti);
    else if (payload.template) await addFromLibrary(payload.template, t, ti);
  }
}

/** Which lane a kind is drawn in, top to bottom. */
const LANE_ORDER: Record<TrackKind, number> = { overlay: 0, video: 1, audio: 2 };

function niceStep(zoom: number): number {
  const target = 80 / zoom; // seconds per ~80px
  const steps = [0.1, 0.25, 0.5, 1, 2, 5, 10, 15, 30, 60];
  return steps.find((s) => s >= target) ?? 60;
}
