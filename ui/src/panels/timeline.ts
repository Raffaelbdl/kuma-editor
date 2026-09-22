// Timeline: tracks, clips (move/trim), transitions, playhead, zoom.

import { button, dragPayload, dragPointer, el, isFileDrag } from "../dom";
import { placeBinItem, addFromLibrary } from "../edits";
import { icon } from "../icons";
import { animationOf, keysOf } from "../keyframes";
import { store } from "../store";
import type { Clip, PatchOp, Project, Track, TrackKind } from "../types";
import { binItem, clipKind, isOverlaySource, isSequenceSource, isTimedMedia, seqDuration, sequenceOf } from "../types";

const HEAD = 136;

/** One clip travelling with a drag: where it started, and the ghost showing where it
 *  would land. */
interface Member { clip: Clip; ti: number; start: number; end: number; ghost: HTMLDivElement }
/** …and where its ghost came to rest, read off at the drop. */
interface Landed { m: Member; start: number; end: number }

export class Timeline {
  private root: HTMLDivElement;
  private playhead: HTMLDivElement | null = null;
  /** A drag in progress. `block` is every clip moving with it, the one under the pointer
   *  first, each with where it started and the ghost showing where it would land. A move's
   *  block is the selection (already the group, if the clip is in one); a trim's is that
   *  clip alone, because a block says *when* these clips play, not how long they are. */
  private drag: null | {
    kind: "move" | "trim-l" | "trim-r"; startX: number;
    /** The dragged clip's source in-point, which a left trim moves with the edge. */
    in: number;
    /** How many tracks the block has been dragged, applied to every member: one offset,
     *  not one destination, since there is no single track two clips land on. */
    shift: number;
    block: Member[];
    /** Every edge of a clip outside the block, to snap to. Fixed for the drag — a block
     *  never snaps to its own members, or it would stick to itself. */
    cuts: number[];
    /** The block's earliest start: it stops when *this* reaches 0, not the pressed clip. */
    floor: number;
    /** The press held a multi-selection together. If it turns out to be a click, that
     *  selection collapses to the clip that was pressed. */
    keep: boolean;
  } = null;

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
    // Toggled rather than rebuilt, like the selection: which track a paste lands on is a
    // click away and has to show.
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
    // Files from the desktop are left to the bin (`Library`).
    host.addEventListener("dragover", (e) => { if (!isFileDrag(e)) e.preventDefault(); });
    host.addEventListener("drop", (e) => { if (!isFileDrag(e)) void this.onDrop(e); });
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

    // Overlays, then video, then audio: the order they composite in, top down, with audio
    // — which composites in no order — out of the way at the bottom. Within a kind the
    // array order is reversed, a later track being the one that composites over. Only the
    // drawing is grouped; `data-ti` still names the document's own order.
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
    // An empty lane says "this track, this moment": both halves of where a paste goes.
    lane.addEventListener("pointerdown", (e) => { if (e.target === lane) { store.selectClips([], t.id); this.scrub(e); } });
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
    const box = el("div", `clip ${clipKind(p, c, t)}${store.selectedClip === c.id ? " selected" : ""}${w < 24 ? " tiny" : ""}${c.group ? " grouped" : ""}`);
    box.dataset.id = c.id;
    box.style.left = `${this.x(c.start) - HEAD}px`;
    box.style.width = `${w}px`;
    const item = binItem(p, c);
    const name = item ? item.name || c.bin : `? ${c.bin}`;
    // The template is worth showing only when the entry was not named after it.
    const template = item && isOverlaySource(item.source) && item.source.template !== name ? item.source.template : "";
    const seq = item && isSequenceSource(item.source) ? `sequence · ${seqDuration(p, c.bin).toFixed(1)}s` : "";
    const sub = c.speed !== 1 ? `${c.speed}×` : template || seq;
    box.title = `${c.id}  ${c.start.toFixed(2)}–${c.end.toFixed(2)}s${c.group ? ` · grouped (${c.group}) — selecting one selects them all, and a drag moves the block` : ""}`;
    const hl = el("div", "handle l");
    const hr = el("div", "handle r");
    const title = el("div", "cname");
    // A chain before the name, or the first surprise is three clips moving when you
    // dragged one.
    if (c.group) title.appendChild(icon("link", 10));
    title.append(c.label || name);
    box.append(title, el("div", "sub", String(sub)), hl, hr);
    // Keyframed effects along the bottom of the clip, so an animated clip says so at a
    // glance. A key can be dragged to another time here, or clicked to go there; its
    // value is the inspector's.
    for (const k of keysOf(c)) {
      const lane = el("div", `kf-lane ${k.type}`);
      k.times.forEach((t, i) => {
        const key = el("div", "kf");
        key.style.left = `${t * store.state.zoom}px`;
        key.title = `${k.type} key at ${(c.start + t).toFixed(2)}s — drag to move it, click to go there`;
        key.addEventListener("pointerdown", (e) => {
          // This press is the key's: the clip under it must not start moving.
          e.stopPropagation();
          const anim = animationOf(c.id, k.index);
          if (!anim) return;
          const zoom = store.state.zoom;
          let at = t;
          dragPointer(e, {
            move: (dx) => {
              at = anim.clampT(t + dx / zoom);
              key.style.left = `${at * zoom}px`;
            },
            end: (moved) => {
              if (moved) anim.moveKey(i, at);
              else store.seek(c.start + t);
            },
          });
        });
        lane.appendChild(key);
      });
      box.appendChild(lane);
    }
    box.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      // Ctrl/Cmd or Shift builds a selection and does not drag: a drag begun on the
      // modifier would pull one clip out of a group that looks joined. The track goes with
      // it, so a copy and a paste with nothing else clicked land back where they came from.
      if (e.ctrlKey || e.metaKey || e.shiftKey) {
        store.toggleClip(c.id, t.id);
        return;
      }
      const kind = e.target === hl ? "trim-l" : e.target === hr ? "trim-r" : "move";
      // Pressing a clip that is already one of several selected **keeps** the selection:
      // that is what makes dragging a multi-selection a gesture at all. Pressing one
      // outside it selects that one first, as a click always did. A press that turns out
      // to be a click and not a drag collapses to it on release — see `endDrag`.
      const keep = store.selectedClips.length > 1 && store.selectedClips.includes(c.id);
      if (keep) store.focusTrack(t.id);
      else store.selectClips([c.id], t.id);
      // Selected first: the selection is what a move takes with it.
      this.beginDrag(e, kind, c, ti, box, keep);
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

  private beginDrag(e: PointerEvent, kind: "move" | "trim-l" | "trim-r", clip: Clip, ti: number, el: HTMLDivElement, keep: boolean) {
    const ghostFor = (box: HTMLElement) => {
      const g = box.cloneNode(true) as HTMLDivElement;
      g.classList.add("ghost");
      box.parentElement!.appendChild(g);
      return g;
    };
    // The clip under the pointer comes first, so everything else can be measured against
    // where *it* started.
    const block = [{ clip, ti, start: clip.start, end: clip.end, ghost: ghostFor(el) }];
    if (kind === "move") {
      for (const id of store.selectedClips) {
        if (id === clip.id) continue;
        const f = store.findClip(id);
        // Only what is drawn can be dragged: a clip on another sequence has no box here.
        const box = this.root.querySelector<HTMLElement>(`.clip[data-id="${id}"]:not(.ghost)`);
        if (f && box) block.push({ clip: f.clip, ti: f.ti, start: f.clip.start, end: f.clip.end, ghost: ghostFor(box) });
      }
    }
    const moving = new Set(block.map((m) => m.clip.id));
    const cuts = store.tracks.flatMap((tr) => tr.clips.filter((c) => !moving.has(c.id)).flatMap((c) => [c.start, c.end]));
    this.drag = { kind, startX: e.clientX, in: clip.in ?? 0, shift: 0, block, cuts, floor: Math.min(...block.map((m) => m.start)), keep };
    const move = (ev: PointerEvent) => this.onDragMove(ev);
    const up = () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up); this.endDrag(); };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  /** Whether the whole block can move `n` tracks. Every member has to land on a track, and
   *  on one of its own kind — an overlay clip on a video track is a document the daemon
   *  refuses, and a block that left one member behind would not be a block. */
  private canShift(block: { ti: number }[], n: number): boolean {
    return block.every((m) => store.tracks[m.ti + n]?.kind === store.tracks[m.ti].kind);
  }

  /** Snap to 0, the playhead or a cut outside the block; else to the frame. */
  private snapT(t: number, cuts: number[]): number {
    const thresh = 8 / store.state.zoom;
    const near = (c: number) => Math.abs(c - t) < thresh;
    if (near(0)) return 0;
    if (near(store.state.playhead)) return store.state.playhead;
    return cuts.find(near) ?? store.snap(t);
  }

  /** Put a ghost where its clip would land, and remember where that is for the drop. */
  private placeGhost(ghost: HTMLDivElement, from: number, to: number) {
    ghost.style.left = `${this.x(from) - HEAD}px`;
    ghost.style.width = `${Math.max(4, (to - from) * store.state.zoom)}px`;
    ghost.dataset.start = String(from);
    ghost.dataset.end = String(to);
  }

  private onDragMove(ev: PointerEvent) {
    const d = this.drag;
    if (!d) return;
    const dt = (ev.clientX - d.startX) / store.state.zoom;
    const me = d.block[0];
    if (d.kind === "move") {
      // Every member travels what the pressed clip does, so the block keeps its shape —
      // held back so that the earliest of them stops at 0 rather than the one under the
      // pointer, or the daemon would refuse the whole patch for a negative start.
      const delta = Math.max(-d.floor, this.snapT(me.start + dt, d.cuts) - me.start);
      // Tracks move by the same rule as time: the row under the pointer says how far the
      // *pressed* clip has travelled, and every member travels that far. Dropping a block
      // "onto" a track is the question with no answer; how many tracks it has come is the
      // one that has. Off the tracks entirely — over the ruler, past the edge — it keeps
      // the offset it had rather than springing back.
      const row = (ev.target as HTMLElement | null)?.closest?.(".tl-track") as HTMLElement | null;
      const want = row ? Number(row.dataset.ti) - me.ti : d.shift;
      const shift = this.canShift(d.block, want) ? want : 0;
      if (shift !== d.shift) {
        d.shift = shift;
        for (const m of d.block) this.root.querySelector(`[data-ti="${m.ti + shift}"] .tl-lane`)?.appendChild(m.ghost);
      }
      for (const m of d.block) this.placeGhost(m.ghost, m.start + delta, m.end + delta);
    } else if (d.kind === "trim-l") {
      let start = Math.min(me.end - 1 / store.fps, Math.max(0, this.snapT(me.start + dt, d.cuts)));
      // Cannot reveal material before the source's first frame (handles scale with speed).
      const speed = Math.abs(me.clip.speed) || 1;
      const timed = isTimedMedia(binItem(store.state.project!, me.clip));
      if (timed && d.in + (start - me.start) * speed < 0) start = store.snap(me.start - d.in / speed);
      this.placeGhost(me.ghost, start, me.end);
    } else {
      this.placeGhost(me.ghost, me.start, Math.max(me.start + 1 / store.fps, this.snapT(me.end + dt, d.cuts)));
    }
  }

  private async endDrag() {
    const d = this.drag;
    this.drag = null;
    if (!d) return;
    const me = d.block[0];
    const landed = d.block.map((m) => ({ m, start: Number(m.ghost.dataset.start ?? m.start), end: Number(m.ghost.dataset.end ?? m.end) }));
    for (const m of d.block) m.ghost.remove();
    const [mine] = landed;
    if (mine.start === me.start && mine.end === me.end && d.shift === 0) {
      // A click, not a drag. The selection was held together in case this became one, so
      // now it collapses to the clip that was pressed — otherwise there would be no way to
      // get back to a single clip except by clicking empty space first.
      if (d.keep) store.selectClips([me.clip.id]);
      return;
    }
    const path = store.clipPath(me.clip.id);
    if (!path) return; // the clip went away mid-drag (undo, agent edit)
    const ops: PatchOp[] = [];
    if (d.kind === "trim-l" && isTimedMedia(binItem(store.state.project!, me.clip))) {
      ops.push({ op: "replace", path: `${path}/in`, value: Math.max(0, d.in + (mine.start - me.start) * Math.abs(me.clip.speed)) });
    }
    if (d.shift !== 0) {
      const shifted = this.shiftOps(landed, d.shift);
      if (!shifted) return;
      ops.push(...shifted);
    } else {
      // The whole block in one patch: half a group having moved is not a state anyone
      // asked for. A member that went away mid-drag is left out, not refused.
      for (const { m, start, end } of landed) {
        const p = store.clipPath(m.clip.id);
        if (p) ops.push({ op: "replace", path: `${p}/start`, value: start }, { op: "replace", path: `${p}/end`, value: end });
      }
    }
    await store.patch(ops, `${d.kind} ${landed.map((l) => l.m.clip.id).join(", ")}`);
  }

  /**
   * The patch for a block that changed track, every member by the same offset. Each
   * affected track's clips are rewritten whole rather than moved one at a time: a remove
   * renumbers the clips after it, and two members can trade tracks, so removes and adds
   * would depend on their own order to come out right.
   *
   * Null when the document moved under the drag, which is one to abandon rather than guess
   * at: a stale index would rewrite a track that has nothing to do with it.
   */
  private shiftOps(landed: Landed[], shift: number): PatchOp[] | null {
    const members = landed.map(({ m }) => m);
    if (!members.every((m) => store.findClip(m.clip.id)?.ti === m.ti) || !this.canShift(members, shift)) return null;
    const gone = new Set(members.map((m) => m.clip.id));
    const arriving = new Map<number, Clip[]>();
    for (const { m, start, end } of landed) arriving.set(m.ti + shift, [...(arriving.get(m.ti + shift) ?? []), { ...m.clip, start, end }]);
    const ops: PatchOp[] = [];
    for (const ti of new Set([...members.map((m) => m.ti), ...arriving.keys()])) {
      const track = store.tracks[ti];
      const tp = `${store.seqPath}/tracks/${ti}`;
      ops.push({ op: "replace", path: `${tp}/clips`, value: [...track.clips.filter((c) => !gone.has(c.id)), ...(arriving.get(ti) ?? [])] });
      // A transition joins two clips on one track, so one whose clip has left goes too.
      const xs = track.transitions.filter((x) => !gone.has(x.from) && !gone.has(x.to));
      if (xs.length !== track.transitions.length) ops.push({ op: "replace", path: `${tp}/transitions`, value: xs });
    }
    return ops;
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
