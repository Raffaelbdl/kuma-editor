import { api, subscribeEvents } from "./api";
import type { Activity, BlueprintInfo, Clip, HistoryEntry, Pin, DaemonEvent, FontFamily, Job, Marker, OverlayInfo, PatchOp, Project, Track, TrackKind, Transition } from "./types";
import { allClips, clipPathOf, fpsValue, freeId, isOverlaySource, nearest, seqDuration, seqPath, sequenceIds, sequenceOf } from "./types";

/** A clip found in the project: where it is, as the record every edit of it starts from. */
export interface Found { seq: string; track: Track; ti: number; clip: Clip; ci: number }

/**
 * What the inspector is showing — the model's own split: what is shown lives on the entry,
 * when it is shown lives on the clip. Clips come as a list because copying, deleting and
 * splitting all act on several at once; a bin entry does not, because every entry action
 * is about *that* entry. The list is in click order, so its last member is the one a
 * single-clip action means.
 */
export type Selection = { kind: "clip"; ids: string[] } | { kind: "bin"; id: string } | { kind: "blueprint"; id: string }
  /** A transition, by its track and the two clips it joins: it has no id of its own. */
  | { kind: "transition"; track: string; from: string; to: string } | null;

/** A clip waiting to be pasted, with the track it came off. The *kind* travels rather than
 *  being derived from the entry — a video file on an audio track is an audio clip — and
 *  the *id* travels because a copy of several keeps them on their own tracks. */
export interface ClipboardEntry { clip: Clip; track: string; kind: TrackKind }

/** What was copied or cut: one clip, or several with the shape they were in. */
export interface Clipboard { entries: ClipboardEntry[] }

/** The tab that shows the sequence being edited. Not a bin id — ids must match
 *  `[A-Za-z_][A-Za-z0-9_-]*`, so the empty string can never name an entry and the two
 *  kinds of tab cannot collide. */
export const SEQUENCE_TAB = "";

export interface State {
  project: Project | null;
  /** Reusable overlays in the user's library, to copy into the project. */
  blueprints: BlueprintInfo[];
  /** The manifest of every overlay folder the bin uses, by folder. */
  manifests: Record<string, OverlayInfo>;
  /** Font families `font` vars can name. */
  fonts: FontFamily[];
  /** Bumped when a preview render finishes: preview images' cache key. */
  previewStamp: number;
  /** The sequence open for editing. The daemon owns this (the preview compiles it), so it
   *  is set through the API rather than locally. Empty until the first load. */
  sequence: string;
  selection: Selection;
  /** The track a paste lands on: the one last clicked, by id, because indices shift when
   *  a track is added. Null until a track has been clicked, and dropped when the one it
   *  names leaves the open sequence. View state, like the selection. */
  track: string | null;
  /** What was last copied or cut. Held here, not in the document: copying is not an edit,
   *  so it costs no revision and survives switching sequences. The paste is the edit. */
  clipboard: Clipboard | null;
  /** Record mode: while on, changing an animatable parameter writes a key at the playhead
   *  instead of the static value. View state — it is about how the next edit is made. */
  autoKey: boolean;
  /** An asked-for preview reload is in flight: it can bake, which takes seconds. */
  reloading: boolean;
  playhead: number;
  playing: boolean;
  /** pixels per second */
  zoom: number;
  /** Which editor the centre shows: `SEQUENCE_TAB` for the sequence monitor, else the bin
   *  id of an overlay entry open in the workbench. View state, like the selection. */
  tab: string;
  /** Sequences open as tabs, oldest first. Only one is *being edited* — `sequence`, which
   *  the daemon owns — so these are the ones a click away rather than several at once. */
  sequences: string[];
  /** Overlay entries open as tabs, oldest first. */
  overlays: string[];
  connected: boolean;
  jobs: Record<number, Job>;
  /** Power (panels docked round the frame) or den (the frame, panels summoned over it).
   *  View state, this browser's (`panelhost.ts`). */
  mode: "power" | "den";
  /** The agent's tool calls, newest first, as the daemon heard of them over MCP. */
  activity: Activity[];
  /** The project's history, newest first: every step, who made it, and refused patches. */
  history: HistoryEntry[];
  /** Pins on every sequence: markers with a comment, and whether a revision answered them. */
  pins: Pin[];
}

type Listener = (s: State) => void;

class Store {
  state: State = { project: null, blueprints: [], manifests: {}, fonts: [], previewStamp: 0, sequence: "", selection: null, track: null, clipboard: null, autoKey: false, reloading: false, playhead: 0, playing: false, zoom: 60, tab: SEQUENCE_TAB, sequences: [], overlays: [], connected: false, jobs: {}, mode: "power", activity: [], history: [], pins: [] };
  private listeners = new Set<Listener>();
  private refreshing: Promise<void> | null = null;
  private refreshAgain = false;

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    fn(this.state);
    return () => this.listeners.delete(fn);
  }

  /**
   * Subscribe to a slice of the state: `fn` runs only when one of the picked values
   * changes (by identity). Use this for anything that rebuilds DOM — the playhead moves
   * 25× per second during playback and must not re-render panels.
   */
  select(pick: (s: State) => unknown[], fn: Listener): () => void {
    let last: unknown[] | null = null;
    return this.subscribe((s) => {
      const now = pick(s);
      if (last && now.length === last.length && now.every((v, i) => v === last![i])) return;
      last = now;
      fn(s);
    });
  }

  set(partial: Partial<State>) {
    this.state = { ...this.state, ...partial };
    // One panel throwing must not stop the others from updating.
    for (const l of this.listeners) {
      try { l(this.state); } catch (e) { console.error(e); }
    }
  }

  async init() {
    const [project, blueprints, manifests, seq] = await Promise.all([api.project(), api.blueprints(), api.overlays(), api.sequence()]);
    this.set({ project, blueprints, manifests, sequence: seq.current, sequences: [seq.current] });
    void this.reloadFonts();
    subscribeEvents((e) => this.onEvent(e), (connected) => {
      this.set({ connected });
      // Events only say what happens from now on: what is already queued or running (or
      // happened while the socket was down) comes from the daemon's own table.
      if (connected) { void this.loadJobs(); void this.loadAgent(); }
    });
  }

  /** The agent's side of the session — its calls, the history, the pins — from the daemon's
   *  own tables, as jobs are: events only say what happens from now on. A daemon older
   *  than these endpoints answers 404, and the panels say there is nothing yet. */
  private async loadAgent() {
    const [a, h] = await Promise.all([api.activity().catch(() => null), api.history().catch(() => null)]);
    this.set({ ...(a ? { activity: a.activity } : {}), ...(h ? { history: h.entries } : {}) });
    // After this, every change to them comes as a `pins` event carrying the list.
    api.pins().then((r) => this.set({ pins: r.pins })).catch(() => {});
  }

  private async loadJobs() {
    try {
      const { jobs } = await api.jobs();
      this.set({ jobs: { ...this.state.jobs, ...Object.fromEntries(jobs.map((j) => [j.id, j])) } });
    } catch { /* an older daemon, or a dropped socket: events still arrive */ }
  }

  /**
   * Open a sequence for editing, and give it a tab. The daemon holds the open sequence and
   * the engine holds its compiled timeline, so this is a round trip rather than a local
   * flag — which is also why the tabs are a list of sequences one click away rather than
   * several open at once. Switching costs a compile; that is the honest price.
   */
  async openSequence(id: string) {
    if (id === this.state.sequence) {
      // Already the one being edited: the click is asking for the monitor, not a reload.
      this.set({ sequences: withId(this.state.sequences, id), tab: SEQUENCE_TAB });
      return;
    }
    // Stop first: the preview is about to show another timeline, and the transport will
    // drive that one. Playback carrying on through the round trip is playback nobody can
    // reach.
    if (this.state.playing) this.set({ playing: false });
    try {
      const { current } = await api.openSequence(id);
      // The daemon has the last word on what is open (it falls back to the master for a
      // sequence that has gone), so the tab follows `current`, not what was asked for.
      // Opening a timeline is asking to look at it: the monitor comes forward with it.
      this.set({ sequence: current, sequences: withId(this.state.sequences, current), selection: null, track: null, playhead: 0, tab: SEQUENCE_TAB });
    } catch (e) {
      toast((e as Error).message, "err");
    }
  }

  /** Close a sequence tab. Only the tab — the sequence stays in the bin, and the picker
   *  reopens it. The last one stays put: the daemon always has a sequence open, so a
   *  strip with none would be a lie about what the preview is showing. */
  async closeSequence(id: string) {
    const rest = this.state.sequences.filter((x) => x !== id);
    if (!rest.length) return;
    this.set({ sequences: rest });
    if (id === this.state.sequence) await this.openSequence(rest[rest.length - 1]);
  }

  private async onEvent(e: DaemonEvent) {
    if (e.type === "project_changed") {
      if (!this.state.project || e.revision !== this.state.project.revision) await this.refresh();
      toast(`${e.origin} → r${e.revision}`, "ok", 1200);
    } else if (e.type === "activity") {
      const { type: _, ...a } = e;
      this.set({ activity: [a, ...this.state.activity.filter((x) => x.id !== a.id)].sort((x, y) => y.id - x.id).slice(0, 100) });
    } else if (e.type === "history") {
      const { type: _, ...h } = e;
      this.set({ history: [h, ...this.state.history].slice(0, 500) });
    } else if (e.type === "pins") {
      this.set({ pins: e.pins });
    } else if (e.type === "jobs_cleared") {
      const jobs = { ...this.state.jobs };
      for (const id of e.ids) delete jobs[id];
      this.set({ jobs });
    } else if (e.type === "job") {
      const { type: _, ...job } = e;
      this.set({ jobs: { ...this.state.jobs, [e.id]: job }, ...(e.kind === "thumbs" && e.state === "done" ? { previewStamp: e.id } : {}) });
      if (e.state === "failed") toast(`${e.kind} failed: ${e.message}`, "err", 12000);
      else if (e.state === "done" || e.state === "cancelled") toast(`${e.kind} ${e.state}`, "ok", 4000);
    }
  }

  /**
   * Refetch the project. A refresh requested while one is in flight runs once more after
   * it, so a burst of revisions (an agent applying several patches) never leaves the UI
   * on a stale one.
   */
  refresh(): Promise<void> {
    if (this.refreshing) {
      this.refreshAgain = true;
      return this.refreshing;
    }
    this.refreshing = (async () => {
      try {
        do {
          this.refreshAgain = false;
          const [project, manifests] = await Promise.all([api.project(), api.overlays().catch(() => this.state.manifests)]);
          // The open sequence can be deleted out from under us (undo, an agent edit); the
          // daemon falls back to the master and so do we, rather than showing nothing.
          const sequence = sequenceOf(project, this.state.sequence) ? this.state.sequence : project.master;
          const selection = this.stillThere(project, this.state.selection);
          // The focused track can go the same way the selection can (undo, an agent edit).
          const track = sequenceOf(project, sequence)?.tracks.some((t) => t.id === this.state.track) ? this.state.track : null;
          // A tab's subject can be removed out from under us (undo, an agent edit), so
          // every tab is checked against the document it names — and the one being
          // edited always has a tab, whether it was opened or fallen back to.
          const sequences = withId(this.state.sequences.filter((id) => sequenceOf(project, id)), sequence);
          const overlays = this.state.overlays.filter((id) => isOverlayEntry(project, id));
          const tab = overlays.includes(this.state.tab) ? this.state.tab : SEQUENCE_TAB;
          this.set({ project, manifests, sequence, sequences, selection, track, overlays, tab });
        } while (this.refreshAgain);
      } catch (e) {
        toast(`Cannot reload the project: ${(e as Error).message}`, "err");
      } finally {
        this.refreshing = null;
      }
    })();
    return this.refreshing;
  }

  /** Re-read the fonts, and the editor's own `@font-face` for them, which is what lets the
   *  picker show each family in its face. A font is a file, not an edit, so nothing else
   *  says when one arrives. */
  async reloadFonts() {
    const fonts = await api.fonts().catch(() => this.state.fonts);
    let link = document.getElementById("kuma-fonts") as HTMLLinkElement | null;
    if (!link) {
      link = document.createElement("link");
      link.id = "kuma-fonts";
      link.rel = "stylesheet";
      document.head.appendChild(link);
    }
    link.href = `/api/fonts.css?v=${Date.now()}`;
    this.set({ fonts });
  }

  /** Re-read the manifests after a folder was edited on disk. */
  async reloadOverlays() {
    this.set({ manifests: await api.overlays() });
  }

  /** A selection survives a refresh only while the things it names still exist — each of
   *  them, so deleting one of three selected clips leaves the other two selected. */
  private stillThere(project: Project, sel: Selection): Selection {
    if (!sel) return null;
    if (sel.kind === "bin") return sel.id in project.bin ? sel : null;
    if (sel.kind === "blueprint") return this.state.blueprints.some((b) => b.id === sel.id) ? sel : null;
    if (sel.kind === "transition") return this.findTransition(project, sel) ? sel : null;
    const live = new Set(allClips(project).map((c) => c.id));
    const ids = sel.ids.filter((id) => live.has(id));
    return ids.length ? { kind: "clip", ids } : null;
  }

  /**
   * Open an overlay entry in the workbench and bring its tab forward — from the bin, the
   * inspector, or its own tab, which is the same act whether or not the tab is already
   * there. Selecting it too is the point rather than a courtesy: the inspector is where a
   * bin entry's vars are edited, and the workbench deliberately does not grow a second
   * copy of them.
   */
  openOverlay(id: string) {
    if (!isOverlayEntry(this.state.project, id)) {
      toast(`${id} is not an overlay entry`, "err");
      return;
    }
    this.set({ overlays: withId(this.state.overlays, id), tab: id, selection: { kind: "bin", id } });
  }

  /** Close a workbench tab. Only the tab: the entry stays in the bin, where it is the
   *  thing every clip plays. */
  closeOverlay(id: string) {
    const overlays = this.state.overlays.filter((x) => x !== id);
    const tab = this.state.tab === id ? overlays.at(-1) ?? SEQUENCE_TAB : this.state.tab;
    this.set({ overlays, tab });
  }

  /** Point the inspector at a clip, a bin entry or a blueprint, and at nothing else.
   *  (`select` above is the subscription helper; these are deliberately not the same word.) */
  selectItem(kind: "clip" | "bin" | "blueprint", id: string) {
    this.set({ selection: kind === "clip" ? { kind: "clip", ids: [id] } : { kind, id } });
  }

  selectTransition(track: string, from: string, to: string) {
    this.set({ selection: { kind: "transition", track, from, to } });
  }

  /** The selected transition's track and where it is — in the open sequence, since that
   *  is the only one a transition can be clicked on. */
  findTransition(project: Project | null, sel: { track: string; from: string; to: string }): { ti: number; track: Track; x: Transition } | null {
    const tracks = project ? sequenceOf(project, this.state.sequence)?.tracks ?? [] : [];
    const ti = tracks.findIndex((t) => t.id === sel.track);
    const x = tracks[ti]?.transitions.find((y) => y.from === sel.from && y.to === sel.to);
    return x ? { ti, track: tracks[ti], x } : null;
  }

  /** Point the inspector at several clips, and aim the next paste at a track in the same
   *  update — clicking a clip is also clicking its track, and that is one notification.
   *  A clip in a group brings its group: expanding here is what stops a gesture forgetting. */
  selectClips(ids: string[], track?: string) {
    const all = this.groupmates(ids);
    this.set({ selection: all.length ? { kind: "clip", ids: all } : null, ...(track === undefined ? {} : { track }) });
  }

  /** Add a clip to the selection, or take it out again: Ctrl/Cmd- or Shift-click. A group
   *  goes in and comes out whole. The one just clicked goes last, so it is the one the
   *  inspector edits. */
  toggleClip(id: string, track?: string) {
    const mates = new Set(this.groupmates([id]));
    const rest = this.selectedClips.filter((x) => !mates.has(x));
    // Nothing dropped means it was not selected, so this click adds it (and its group).
    this.selectClips(rest.length === this.selectedClips.length ? [...rest, id] : rest, track);
  }

  /** These clips and everything grouped with them, on the open sequence. The ones asked
   *  for keep their order and stay last, because the last clicked is the one a single-clip
   *  action means. */
  groupmates(ids: string[]): string[] {
    const asked = new Set(ids);
    const clips = this.tracks.flatMap((t) => t.clips);
    const groups = new Set(clips.filter((c) => c.group && asked.has(c.id)).map((c) => c.group));
    if (!groups.size) return ids;
    return [...clips.filter((c) => c.group && groups.has(c.group) && !asked.has(c.id)).map((c) => c.id), ...ids];
  }

  /** Aim the next paste at a track: a paste needs a *where* as well as a *when*, and the
   *  playhead only answers the second half. */
  focusTrack(id: string | null) {
    this.set({ track: id });
  }

  /** Every selected clip, in click order. Empty when a bin entry (or nothing) is selected. */
  get selectedClips(): string[] {
    return this.state.selection?.kind === "clip" ? this.state.selection.ids : [];
  }

  /** The clip a single-clip action means: the last one clicked. */
  get selectedClip(): string | null {
    return this.selectedClips.at(-1) ?? null;
  }

  async undo() { await api.undo(); await this.refresh(); }
  async redo() { await api.redo(); await this.refresh(); }

  /** Send a patch against the current revision. On a 409, refetch and report. */
  async patch(ops: PatchOp[], note: string): Promise<boolean> {
    const p = this.state.project;
    if (!p) return false;
    try {
      await api.patch(ops, p.revision, `ui: ${note}`);
      await this.refresh();
      return true;
    } catch (e) {
      await this.refused(e);
      return false;
    }
  }

  /** Say why the daemon refused an edit; on a 409, refetch first. */
  async refused(e: unknown): Promise<void> {
    const err = e as Error & { status?: number; body?: { issues?: { severity?: string; message: string }[] } };
    if (err.status === 409) {
      toast("Project changed elsewhere — reloaded. Try again.", "err");
      await this.refresh();
    } else if (err.status === 422) {
      const issues = (err.body?.issues ?? []).filter((i) => i.severity !== "warning");
      toast(`Rejected: ${issues.map((i) => i.message).join("; ") || err.message}`, "err", 8000);
    } else {
      toast(err.message, "err");
    }
  }

  // ---- helpers -------------------------------------------------------------

  get fps(): number {
    return this.state.project ? fpsValue(this.state.project.fps) : 30;
  }

  /** Duration of the sequence being edited, not of the whole project. */
  get duration(): number {
    const p = this.state.project;
    return p ? seqDuration(p, this.state.sequence) : 0;
  }

  /** The tracks on screen: those of the open sequence. */
  get tracks(): Track[] {
    const p = this.state.project;
    return (p && sequenceOf(p, this.state.sequence)?.tracks) ?? [];
  }

  /** The markers on screen: those of the open sequence. */
  get markers(): Marker[] {
    const p = this.state.project;
    return (p && sequenceOf(p, this.state.sequence)?.markers) ?? [];
  }

  /** JSON pointer to the open sequence's timeline — the prefix for every track edit. */
  get seqPath(): string {
    return seqPath(this.state.sequence);
  }

  /** Find a clip anywhere in the project; ids are unique across every sequence. Stops at
   *  the match rather than building the whole project's clip list: the preview calls this
   *  for every visible overlay on every frame. */
  findClip(id: string | null): Found | null {
    const p = this.state.project;
    if (!p || !id) return null;
    for (const seq of sequenceIds(p)) {
      const tracks = sequenceOf(p, seq)!.tracks;
      for (let ti = 0; ti < tracks.length; ti++) {
        const ci = tracks[ti].clips.findIndex((c) => c.id === id);
        if (ci >= 0) return { seq, track: tracks[ti], ti, clip: tracks[ti].clips[ci], ci };
      }
    }
    return null;
  }

  /** The clips of `ids` that are in the project, in the same order; the rest are skipped. */
  findClips(ids: string[]): Found[] {
    return ids.map((id) => this.findClip(id)).filter((f): f is Found => f !== null);
  }

  /** The selected clips as records, in click order. */
  get selectedFound(): Found[] {
    return this.findClips(this.selectedClips);
  }

  clipPath(id: string): string | null {
    const f = this.findClip(id);
    return f ? clipPathOf(f) : null;
  }

  /** A clip id free across the whole project: ids are global, not per sequence. `minted`
   *  names the ids a patch has already taken but not yet added. */
  newId(prefix: string, minted: Iterable<string> = []): string {
    return freeId(prefix, [...allClips(this.state.project!).map((c) => c.id), ...minted]);
  }

  /** A group id free across the whole project: minting project-wide keeps a pasted group
   *  from meeting its twin on another timeline. */
  newGroupId(minted: Iterable<string> = []): string {
    const p = this.state.project!;
    return freeId("g", [...allClips(p).flatMap((c) => (c.group ? [c.group] : [])), ...minted]);
  }

  /** A track id free across the whole project, for the same reason, `minted` included. */
  newTrackId(prefix: string, minted: Iterable<string> = []): string {
    const p = this.state.project!;
    return freeId(prefix, [...sequenceIds(p).flatMap((id) => sequenceOf(p, id)!.tracks.map((t) => t.id)), ...minted]);
  }

  /** Half a frame: the tolerance for "on the same frame", since the playhead is on the
   *  frame grid and a keyframe or a cut need not be. */
  get halfFrame(): number {
    return 0.5 / this.fps;
  }

  /** Round to the frame grid. */
  snap(t: number): number {
    const f = this.fps;
    return Math.max(0, Math.round(t * f) / f);
  }

  /** Jump to the nearest cut before or after the playhead — Alt+←/→. A cut is any clip's
   *  start or end on the open sequence, whichever track, plus 0. Nothing that way means
   *  staying put. */
  seekEdge(dir: -1 | 1) {
    const edges = [0, ...this.tracks.flatMap((t) => t.clips.flatMap((c) => [c.start, c.end]))];
    const t = nearest(edges, this.state.playhead, dir, this.halfFrame);
    if (t !== undefined) this.seek(t);
  }

  /** Move the playhead; it may sit past the last clip (that is where new clips go). */
  seek(t: number) {
    this.set({ playhead: this.snap(Math.min(Math.max(0, t), this.duration + 10)) });
  }
}

export const store = new Store();

/** `ids` with `id` in it, appended if it was not — how a tab strip gains one. */
const withId = (ids: string[], id: string): string[] => (ids.includes(id) ? ids : [...ids, id]);


/** An entry the workbench can open: one that exists and is an overlay. */
export const isOverlayEntry = (p: Project | null, id: string): boolean =>
  !!p && !!p.bin[id] && isOverlaySource(p.bin[id].source);

// ---- toasts ------------------------------------------------------------------

export function toast(text: string, kind: "ok" | "err" | "" = "", ms = 3500) {
  const host = document.getElementById("toasts")!;
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = text;
  host.appendChild(el);
  setTimeout(() => el.remove(), ms);
}

/** Start a background job and say so. Its outcome arrives as a job event and, for jobs
 *  that edit, as a project change; nothing here waits for it. */
export async function startJob(label: string, start: () => Promise<{ job: number }>): Promise<void> {
  try {
    const r = await start();
    toast(`${label} job #${r.job} started`, "ok");
  } catch (e) {
    toast((e as Error).message, "err");
  }
}

/** What a bin entry is called: its name, else its id. */
export const binName = (id?: string) => (id && store.state.project?.bin[id]?.name) || id || "?";
/** A sequence as a list offers it, the master said so. */
export const sequenceLabel = (id: string) => `${binName(id)}${id === store.state.project?.master ? " (master)" : ""}`;

/** `mm:ss.ff`, computed from whole frames so the frame field never reaches `fps`. */
export const fmtTime = (t: number, fps: number) => {
  const frames = Math.max(0, Math.round(t * fps));
  const whole = Math.floor(frames / fps);
  const fr = frames - Math.round(whole * fps);
  return `${String(Math.floor(whole / 60)).padStart(2, "0")}:${String(whole % 60).padStart(2, "0")}.${String(fr).padStart(2, "0")}`;
};
