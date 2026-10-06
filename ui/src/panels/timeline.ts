// Timeline: tracks, clips (move/trim), transitions, playhead, zoom.

import { Cmd, run } from "../commands";
import { button, dragPayload, dragPointer, el, isFileDrag, loadLocalJSON, options, saveLocal } from "../dom";
import { addTransition, renameTrack, editMarker, editTransitionDuration, joinedAfter, moveMarker, placeBinItem, placeBlueprint, removeMarker, removeTransition, setTransitionKind, splitAt, transitionBlocked } from "../edits";
import { showMenu } from "../menu";
import { icon } from "../icons";
import { animationOf, keysOf } from "../keyframes";
import { keysFor } from "../shortcuts";
import { panels } from "../panelhost";
import { store } from "../store";
import type { Clip, Marker, PatchOp, Project, Track, TrackKind, Transition } from "../types";
import { binItem, clipKind, hasAudio, isFileSource, isOverlaySource, isSequenceSource, isTimedMedia, seqDuration, sourceAfter } from "../types";
import { drawStrip, drawWave } from "./clipmedia";

const HEAD = 150;

/** Track heights, by kind, until one is dragged; a folded track is one line. */
const TRACK_H = 36;
const FOLDED_H = 22;
const MIN_H = 30;
const MAX_H = 240;
/** What a clip's box takes of its row's height: `.clip`'s 4px top and bottom, and its
 *  1px border — the pictures inside are drawn for what is left. */
const CLIP_CHROME = 10;
/** Where heights and folds are kept: view state, this browser's, by track id (ids are
 *  unique across the project). Never the document — how tall a track is drawn is not
 *  something an agent or a render reads. */
const ROWS_KEY = "kuma.timeline.rows";
interface Rows { h: Record<string, number>; folded: string[] }

/** One clip travelling with a drag: where it started, and the ghost showing where it
 *  would land. */
interface Member { clip: Clip; ti: number; start: number; end: number; ghost: HTMLDivElement }
/** …and where its ghost came to rest, read off at the drop. */
interface Landed { m: Member; start: number; end: number }

/** Pixels per second, at the widest and the closest. */
const ZOOM_MIN = 8;
const ZOOM_MAX = 600;
const clampZoom = (z: number) => Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, z));

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

  private rows: Rows = loadLocalJSON<Rows>(ROWS_KEY, { h: {}, folded: [] });

  private saveRows() {
    saveLocal(ROWS_KEY, JSON.stringify(this.rows));
  }

  /** The waveform's colour, the theme's, read once a render rather than once a clip. */
  private waveColor = "";

  private folded(t: Track) { return this.rows.folded.includes(t.id); }
  private heightOf(t: Track) { return this.folded(t) ? FOLDED_H : this.rows.h[t.id] ?? TRACK_H; }

  private zoomInput = el("input");
  private cornerEl = this.corner();

  constructor(private host: HTMLElement) {
    this.root = el("div", "tl");
    store.select((s) => [s.zoom], (s) => {
      this.zoomInput.value = String(s.zoom);
      this.zoomInput.title = `Zoom: ${Math.round(s.zoom)} px a second (Ctrl + wheel zooms round the pointer)`;
    });
    host.appendChild(this.root);
    // Only the project, the open sequence and the zoom rebuild the DOM; selection and
    // playhead are cheap updates.
    // Not while it is out of sight (den, another tab): it is thousands of elements.
    const render = panels.lazily("timeline", () => this.render());
    store.select((s) => [s.project, s.sequence, s.zoom], render.run);
    store.select((s) => [s.selection], () => {
      const sel = new Set(store.selectedClips);
      for (const c of this.root.querySelectorAll<HTMLElement>(".clip:not(.ghost)")) c.classList.toggle("selected", sel.has(c.dataset.id!));
      for (const x of this.root.querySelectorAll<HTMLElement>(".tl-xfade")) x.classList.toggle("selected", this.isSelected(x.dataset.track!, x.dataset.from!, x.dataset.to!));
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
        // Around the pointer: the moment under it stays under it, as in every editor.
        const t = Math.max(0, this.t(e.clientX - this.root.getBoundingClientRect().left));
        const at = e.clientX - host.getBoundingClientRect().left;
        const z = clampZoom(store.state.zoom * (e.deltaY < 0 ? 1.15 : 1 / 1.15));
        store.set({ zoom: z });
        host.scrollLeft = this.x(t) - at;
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
    this.waveColor = getComputedStyle(document.documentElement).getPropertyValue("--clip-sub").trim() || "#fffb";

    // Ruler
    const ruler = el("div", "tl-ruler");
    const step = niceStep(s.zoom);
    const end = store.duration + 10;
    for (let i = 0; i * step <= end; i++) {
      const t = i * step; // by index, so float drift never turns "3s" into "3.0"
      const tick = el("div", "tick", Number.isInteger(t) ? `${t}` : t.toFixed(1));
      tick.style.left = `${this.x(t)}px`;
      ruler.appendChild(tick);
    }
    ruler.addEventListener("pointerdown", (e) => { if (e.button === 0) this.scrub(e); });
    ruler.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      const at = store.snap(Math.max(0, this.t(e.clientX - this.root.getBoundingClientRect().left)));
      showMenu([
        { label: "Marker here", run: () => run(Cmd.addMarker, at), title: "A note on this moment, for the agent or for you — or none" },
        { label: "Move the playhead here", run: () => store.seek(at) },
      ], e.clientX, e.clientY);
    });
    // The same element every render: a rebuild mid-drag would drop the slider's grip.
    ruler.appendChild(this.cornerEl);
    this.root.appendChild(ruler);
    // A marker's flag, the part you hold, is in the ruler; its line runs down the tracks,
    // drawn after them so it lies over the clips.
    const lines = store.markers.map((m, i) => {
      const [flag, line] = this.markerEl(m, i);
      ruler.appendChild(flag);
      return line;
    });

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

    this.root.append(...lines);
    this.playhead = el("div", "tl-playhead");
    this.root.appendChild(this.playhead);
    this.movePlayhead(s.playhead);
  }

  /** The ruler's first cell, over the heads: the zoom, and a track to add. It stays put
   *  while the timeline scrolls sideways (sticky), as the heads do. */
  private corner(): HTMLDivElement {
    const corner = el("div", "tl-corner");
    corner.addEventListener("pointerdown", (e) => e.stopPropagation());
    const zoomBy = (f: number) => store.set({ zoom: clampZoom(store.state.zoom * f) });
    const zoom = this.zoomInput;
    // A slider wearing the word: "zoom" until you drag it, as the design writes it.
    zoom.type = "range"; zoom.min = String(ZOOM_MIN); zoom.max = String(ZOOM_MAX); zoom.step = "1";
    zoom.className = "zoom";
    zoom.oninput = () => store.set({ zoom: Number(zoom.value) });
    const add = el("select");
    add.title = "Add a track";
    options(add, ["video", "overlay", "audio"].map((value) => ({ value })), undefined, "+ track");
    add.onchange = () => {
      const kind = add.value as TrackKind | "";
      add.value = "";
      if (kind) run(Cmd.addTrack, kind);
    };
    const word = el("span", "zoom-word", "zoom");
    word.appendChild(zoom);
    corner.append(button("−", () => zoomBy(1 / 1.25), "Zoom out"), word, button("+", () => zoomBy(1.25), "Zoom in"), add);
    return corner;
  }

  /**
   * A marker's flag and line. The flag is its comment, cut short, where the ruler can show it. Click goes there,
   * a drag moves it, a double-click edits the comment and a right-click offers the rest.
   * The comment is written for whoever reads the project next — an agent told "see the
   * markers" reads exactly this text.
   */
  private markerEl(m: Marker, i: number): [HTMLDivElement, HTMLDivElement] {
    const flag = el("div", "tl-marker-flag", m.label.split("\n")[0]);
    const line = el("div", "tl-marker");
    flag.dataset.i = String(i);
    const place = (t: number) => { flag.style.left = line.style.left = `${this.x(t)}px`; };
    place(m.t);
    if (m.color) for (const n of [flag, line]) n.style.setProperty("--c", m.color);
    flag.title = `${m.label || "(no comment)"}\n\n${m.t.toFixed(2)}s — drag to move, double-click to edit, right-click for more`;
    flag.addEventListener("pointerdown", (e) => {
      // The flag's press, not the ruler's scrub.
      e.stopPropagation();
      if (e.button !== 0) return;
      const zoom = store.state.zoom;
      // Snaps as a clip does: to 0, the playhead or a cut, else to the frame.
      const cuts = store.tracks.flatMap((tr) => tr.clips.flatMap((c) => [c.start, c.end]));
      let at = m.t;
      dragPointer(e, {
        move: (dx) => {
          at = this.snapT(m.t + dx / zoom, cuts);
          place(at);
        },
        end: (moved) => {
          if (moved) void moveMarker(i, at);
          else store.seek(m.t);
        },
      });
    });
    flag.addEventListener("dblclick", (e) => { e.stopPropagation(); void editMarker(i); });
    flag.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      showMenu([
        { label: "Edit comment…", run: () => void editMarker(i) },
        { label: "Go to marker", run: () => store.seek(m.t) },
        "separator",
        { label: "Delete marker", run: () => void removeMarker(i) },
      ], e.clientX, e.clientY);
    });
    return [flag, line];
  }

  private movePlayhead(t: number) {
    // A transform, not `left`: it moves 25 times a second and must not lay the timeline out.
    if (this.playhead) this.playhead.style.transform = `translateX(${this.x(t)}px)`;
  }

  private trackEl(p: Project, t: Track, ti: number): HTMLDivElement {
    const folded = this.folded(t);
    const row = el("div", `tl-track ${t.kind}${store.state.track === t.id ? " focus" : ""}${folded ? " folded" : ""}`);
    row.dataset.ti = String(ti);
    row.dataset.tid = t.id;
    row.style.height = `${this.heightOf(t)}px`;
    // Too short for a second line: the clip says its name and nothing under it.
    row.classList.toggle("short", this.heightOf(t) < 44);
    const head = el("div", "tl-head");
    head.title = `${t.name || t.id} — click a track to paste onto it (Ctrl/Cmd+V pastes at the playhead)`;
    // Anywhere on the head, buttons included: clicking a track is how a paste is aimed.
    head.addEventListener("pointerdown", () => store.focusTrack(t.id));
    const label = el("div", "label");
    const fold = button(folded ? "▸" : "▾", () => this.setFold(t, !folded), folded ? "Unfold this track" : "Fold this track to one line", "fold");
    const tname = el("div", "tname", t.name || t.id);
    tname.title = "Double-click to rename";
    tname.addEventListener("dblclick", () => void renameTrack(t.id));
    label.append(fold, el("div", "kind", kindCode(store.tracks, t)), tname);
    head.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      showMenu([
        { heading: t.name || t.id },
        { label: "Rename…", run: () => void renameTrack(t.id) },
        { label: folded ? "Unfold" : "Fold to one line", run: () => this.setFold(t, !folded) },
        "separator",
        { label: "Remove track", run: () => void store.patch([{ op: "remove", path: `${store.seqPath}/tracks/${ti}` }], `remove track ${t.id}`), disabled: t.clips.length > 0, title: t.clips.length ? "Only an empty track can be removed" : undefined },
      ], e.clientX, e.clientY);
    });
    const toggles = el("div", "toggles");
    const toggle = (text: string, on: boolean, field: "muted" | "hidden") => {
      const b = button(text, () => store.patch([{ op: "replace", path: `${store.seqPath}/tracks/${ti}/${field}`, value: !on }], `${field} ${t.id}`), field === "muted" ? "Mute" : "Hide");
      b.className = on ? "on" : "";
      return b;
    };
    if (t.kind !== "overlay") toggles.appendChild(toggle("m", t.muted, "muted"));
    if (t.kind !== "audio") toggles.appendChild(toggle("h", t.hidden, "hidden"));
    if (!t.clips.length) toggles.appendChild(button("×", () => store.patch([{ op: "remove", path: `${store.seqPath}/tracks/${ti}` }], `remove track ${t.id}`), "Remove this empty track"));
    head.append(label, toggles, this.resizer(row, t));
    row.appendChild(head);

    const lane = el("div", "tl-lane");
    // An empty lane says "this track, this moment": both halves of where a paste goes.
    lane.addEventListener("pointerdown", (e) => { if (e.target === lane && e.button === 0) { store.selectClips([], t.id); this.scrub(e); } });
    for (const c of t.clips) lane.appendChild(this.clipEl(p, t, ti, c));
    for (const x of t.transitions) {
      const a = t.clips.find((c) => c.id === x.from);
      const b = t.clips.find((c) => c.id === x.to);
      if (a && b) lane.appendChild(this.transitionEl(p, t, ti, x, a, b));
    }
    row.appendChild(lane);
    return row;
  }

  private setFold(t: Track, fold: boolean) {
    this.rows.folded = this.rows.folded.filter((id) => id !== t.id).concat(fold ? [t.id] : []);
    this.saveRows();
    this.render();
  }

  /** The bottom edge of a track's head: drag it for a taller or shorter track, below the
   *  least that still shows a clip's name and picture and it folds; double-click resets. */
  private resizer(row: HTMLElement, t: Track): HTMLDivElement {
    const grip = el("div", "tl-resize");
    grip.title = "Drag to resize this track — double-click to reset";
    grip.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      if (e.button !== 0) return;
      const from = this.heightOf(t);
      let h = from;
      dragPointer(e, {
        axes: "xy",
        move: (_dx, dy) => {
          h = Math.min(MAX_H, Math.max(FOLDED_H, from + dy));
          row.style.height = `${h}px`;
        },
        // Redrawn at the new size (`setFold` renders): the clips' pictures are drawn for
        // their height.
        end: (moved) => {
          if (!moved) return;
          if (h >= MIN_H) this.rows.h[t.id] = Math.round(h);
          this.setFold(t, h < MIN_H);
        },
      });
    });
    grip.addEventListener("dblclick", () => {
      delete this.rows.h[t.id];
      this.setFold(t, false);
    });
    return grip;
  }

  /**
   * A transition, over the cut it joins: a double-click sets its length and a right-click
   * offers the rest. Made from a clip's right-click ("Transition into the next clip").
   *
   * The clips stay butt-joined in the document — that is what keeps a transition one
   * field for an agent — and the compiler overlaps them by reaching past their edit points
   * into the *handles*: `from` plays on past its out point, `to` starts before its in
   * point. Those frames are nowhere else on screen, so the transition draws them, faintly:
   * the incoming clip's handle left of the cut, the outgoing clip's right of it. It reads
   * as the overlap it is. Red when `from` runs out of source (the tail is black): the
   * validator's warning, said where it happens.
   */
  private isSelected(track: string, from: string, to: string) {
    const s = store.state.selection;
    return s?.kind === "transition" && s.track === track && s.from === from && s.to === to;
  }

  private transitionEl(p: Project, t: Track, ti: number, x: Transition, a: Clip, b: Clip): HTMLDivElement {
    const half = x.duration / 2;
    const zoom = store.state.zoom;
    const tail = sourceAfter(p, a);
    const short = tail !== undefined && tail < half * Math.abs(a.speed || 1) - 1e-6;
    const xf = el("div", `tl-xfade ${x.kind}${short ? " short" : ""}${this.isSelected(t.id, x.from, x.to) ? " selected" : ""}`);
    Object.assign(xf.dataset, { track: t.id, from: x.from, to: x.to });
    xf.style.left = `${this.x(b.start - half) - HEAD}px`;
    xf.style.width = `${x.duration * zoom}px`;
    const secs = +x.duration.toFixed(3);
    xf.title = `${x.kind} ${secs}s — click to inspect. The two clips overlap here: “${b.label || b.bin}” starts early and “${a.label || a.bin}” plays on past its end, using the footage beyond their edit points`
      + (short ? `\n\n“${a.label || a.bin}” has only ${tail!.toFixed(2)}s of source after its out point: the end of the transition will be black. Trim its end, or shorten the transition.` : "")
      + "\n\nDouble-click for its length, right-click for more";
    if (!this.folded(t)) {
      const h = Math.round((this.heightOf(t) - CLIP_CHROME) / 2);
      const w = half * zoom;
      /** `c`'s handle from source second `from`, `width` px of it — the rest is black,
       *  as it will render. */
      const handle = (c: Clip, from: number, side: "in" | "out", width: number) => {
        const item = binItem(p, c);
        if (!item || !isFileSource(item.source) || item.source.kind === "audio" || t.kind === "audio") return;
        const band = el("div", `xf-handle ${side}`);
        drawStrip(band, c.bin, item.source.path, { ...c, in: from }, zoom, width, h);
        xf.appendChild(band);
      };
      const speed = (c: Clip) => Math.abs(c.speed || 1);
      handle(b, (b.in ?? 0) - half * speed(b), "in", w);
      handle(a, (a.in ?? 0) + (a.end - a.start) * speed(a), "out", short ? Math.max(0, (tail! / speed(a)) * zoom) : w);
    }
    // A click selects it, for the inspector: its kind, length, pattern and handles.
    xf.addEventListener("pointerdown", (e) => { e.stopPropagation(); if (e.button === 0) store.selectTransition(t.id, x.from, x.to); });
    xf.addEventListener("dblclick", (e) => { e.stopPropagation(); void editTransitionDuration(ti, x); });
    xf.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      e.stopPropagation();
      showMenu([
        { heading: `${x.kind === "wipe" ? "Wipe" : "Dissolve"} · ${secs}s` },
        { label: "Length…", run: () => void editTransitionDuration(ti, x) },
        x.kind === "wipe"
          ? { label: "Make it a dissolve", run: () => void setTransitionKind(ti, x, "dissolve") }
          : { label: "Make it a wipe", run: () => void setTransitionKind(ti, x, "wipe"), title: "A wipe's pattern and softness are in the project file (`luma`, `softness`)" },
        "separator",
        { label: "Remove transition", run: () => void removeTransition(ti, x) },
      ], e.clientX, e.clientY);
    });
    return xf;
  }

  /** A clip's right-click: what can be done to it, at the moment under the pointer. */
  private clipMenu(e: MouseEvent, t: Track, c: Clip) {
    e.preventDefault();
    e.stopPropagation();
    // Right-clicking one of several selected acts on them all, as a key would; one outside
    // the selection is selected first.
    if (!store.selectedClips.includes(c.id)) store.selectClips([c.id], t.id);
    const at = store.snap(this.t(e.clientX - this.root.getBoundingClientRect().left));
    const prev = t.clips.find((a) => joinedAfter(t, a)?.id === c.id);
    /** A transition leaving `a`: offered when `transitionBlocked` has nothing to say. */
    const transition = (label: string, a: Clip | undefined) => {
      const blocked = a ? transitionBlocked(t, a) : "No clip ends where this one starts";
      return { label, run: () => void addTransition(a!.id), disabled: !!blocked, title: blocked ?? "A one-second dissolve across the cut, or as long as the clips allow" };
    };
    const n = store.selectedClips.length;
    const grouped = store.selectedFound.some((f) => f.clip.group);
    showMenu([
      { label: n > 1 ? `Split ${n} clips here` : "Split here", keys: keysFor(Cmd.split), run: () => void splitAt(at), disabled: !(at > c.start && at < c.end) },
      "separator",
      transition("Transition into the next clip", c),
      transition("Transition from the previous clip", prev),
      "separator",
      { label: "Copy", keys: "Ctrl + C", run: () => run(Cmd.copyClip) },
      { label: "Cut", keys: "Ctrl + X", run: () => run(Cmd.cutClip) },
      grouped ? { label: "Ungroup", keys: keysFor(Cmd.ungroup), run: () => run(Cmd.ungroup) } : { label: "Group", keys: keysFor(Cmd.group), run: () => run(Cmd.group), disabled: n < 2 },
      "separator",
      { label: n > 1 ? `Delete ${n} clips` : "Delete", keys: keysFor(Cmd.deleteClip), run: () => run(Cmd.deleteClip) },
    ], e.clientX, e.clientY);
  }

  /** A media clip's pictures: frames above, the waveform below (or alone, for sound). */
  private mediaEl(p: Project, t: Track, c: Clip, w: number): HTMLDivElement | null {
    const item = binItem(p, c);
    if (!item || !isFileSource(item.source) || this.folded(t)) return null;
    const { path, kind } = item.source;
    const h = this.heightOf(t) - CLIP_CHROME;
    const frames = kind !== "audio" && t.kind !== "audio";
    const sound = isTimedMedia(item) && hasAudio(item) && h >= 24;
    if (!frames && !sound) return null;
    const media = el("div", `media${frames && sound ? " both" : ""}`);
    const waveH = frames ? Math.round(h * 0.4) : h;
    if (frames) {
      const band = el("div", "strip");
      drawStrip(band, c.bin, path, c, store.state.zoom, w, sound ? h - waveH : h);
      media.appendChild(band);
    }
    if (sound) {
      const wave = el("canvas", "wave");
      wave.style.height = `${waveH}px`;
      drawWave(wave, c.bin, path, c, store.state.zoom, w, waveH, this.waveColor);
      media.appendChild(wave);
    }
    return media;
  }

  private clipEl(p: Project, t: Track, ti: number, c: Clip): HTMLDivElement {
    const w = Math.max(4, (c.end - c.start) * store.state.zoom);
    const box = el("div", `clip ${clipKind(p, c, t)}${store.selectedClips.includes(c.id) ? " selected" : ""}${w < 24 ? " tiny" : ""}${c.group ? " grouped" : ""}${c.muted ? " silenced" : ""}`);
    box.dataset.id = c.id;
    box.style.left = `${this.x(c.start) - HEAD}px`;
    box.style.width = `${w}px`;
    const item = binItem(p, c);
    const name = item ? item.name || c.bin : `? ${c.bin}`;
    // The overlay's folder is worth showing only when the entry was not named after it.
    const folder = item && isOverlaySource(item.source) ? item.source.overlay.split("/").pop() ?? "" : "";
    const overlay = folder !== name ? folder : "";
    const seq = item && isSequenceSource(item.source) ? `sequence · ${seqDuration(p, c.bin).toFixed(1)}s` : "";
    const sub = c.speed !== 1 ? `${c.speed}×` : overlay || seq;
    box.title = `${c.id}  ${c.start.toFixed(2)}–${c.end.toFixed(2)}s${c.group ? ` · grouped (${c.group}) — selecting one selects them all, and a drag moves the block` : ""}${c.muted ? " · muted — this clip only; its track's M is for the whole track" : ""}`;
    const hl = el("div", "handle l");
    const hr = el("div", "handle r");
    const title = el("div", "cname");
    // A chain before the name, or the first surprise is three clips moving when you
    // dragged one.
    if (c.group) title.appendChild(icon("link", 10));
    // A clip muted on its own says so where its name is: the track's M is off, and a
    // silent clip under an unlit M otherwise reads as a bug.
    if (c.muted) {
      const badge = icon("volume-off", 10);
      badge.classList.add("mute-badge");
      title.appendChild(badge);
    }
    title.append(c.label || name);
    const media = this.mediaEl(p, t, c, w);
    if (media) box.appendChild(media);
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
    box.addEventListener("contextmenu", (e) => this.clipMenu(e, t, c));
    box.addEventListener("pointerdown", (e) => {
      e.stopPropagation();
      // Right and middle buttons are not a press on the clip: a right-click keeps the
      // selection its menu acts on.
      if (e.button !== 0) return;
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
    else if (payload.blueprint) await placeBlueprint(payload.blueprint, t, ti);
  }
}

/** Which lane a kind is drawn in, top to bottom. */
const LANE_ORDER: Record<TrackKind, number> = { overlay: 0, video: 1, audio: 2 };

/** A track's short code in its head: `ov` for an overlay track, `v1`/`a1` for the first
 *  video or audio track, counted the way they are drawn (the bottom one first). */
function kindCode(tracks: Track[], t: Track): string {
  if (t.kind === "overlay") return "ov";
  const same = tracks.filter((x) => x.kind === t.kind);
  return `${t.kind === "video" ? "v" : "a"}${same.indexOf(t) + 1}`;
}

function niceStep(zoom: number): number {
  const target = 80 / zoom; // seconds per ~80px
  const steps = [0.1, 0.25, 0.5, 1, 2, 5, 10, 15, 30, 60];
  return steps.find((s) => s >= target) ?? 60;
}

