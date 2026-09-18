import { api, subscribeEvents } from "./api";
import type { Clip, DaemonEvent, Job, PatchOp, Project, TemplateInfo, Track, TrackKind } from "./types";
import { allClips, fpsValue, freeId, isOverlaySource, seqDuration, seqPath, sequenceIds, sequenceOf } from "./types";

/**
 * What the inspector is showing. A clip and a bin entry are different things to inspect,
 * which is the model's own split: what is shown lives on the entry, when it is shown
 * lives on the clip. One selection addresses either, so the panel never has to guess.
 *
 * Clips come as a list, because copying, deleting and splitting are all things you want
 * to do to several at once; a bin entry does not, because every entry action (rename,
 * vars, delete) is about *that* entry. The list is in click order, so its last member is
 * the one a single-clip action — the inspector's fields, a transcript — means.
 */
export type Selection = { kind: "clip"; ids: string[] } | { kind: "bin"; id: string } | null;

/** A clip taken off a timeline, waiting to be pasted, with the track it came off. The
 *  track's *kind* travels rather than being derived from the bin entry: a video file
 *  placed on an audio track is an audio clip, and that is where a paste of it belongs.
 *  Its *id* travels because a copy of several clips keeps them on their own tracks. */
export interface ClipboardEntry { clip: Clip; track: string; kind: TrackKind }

/** What was copied or cut: one clip, or several with the shape they were in. */
export interface Clipboard { entries: ClipboardEntry[] }

/** The tab that shows the sequence being edited. Not a bin id — ids must match
 *  `[A-Za-z_][A-Za-z0-9_-]*`, so the empty string can never name an entry and the two
 *  kinds of tab cannot collide. */
export const SEQUENCE_TAB = "";

export interface State {
  project: Project | null;
  templates: TemplateInfo[];
  /** The sequence open for editing. The daemon owns this (the preview compiles it), so it
   *  is set through the API rather than locally. Empty until the first load. */
  sequence: string;
  selection: Selection;
  /** The track a paste lands on: the one last clicked, by id, because indices shift when
   *  a track is added. Null until a track has been clicked, and dropped when the one it
   *  names leaves the open sequence. View state, like the selection. */
  track: string | null;
  /** The clip last copied or cut, if any. It is held here rather than in the document:
   *  copying is not an edit, so it costs no revision and no undo step — the paste is the
   *  edit. It survives switching sequences, which is what makes pasting into another
   *  timeline the same gesture as pasting into this one. */
  clipboard: Clipboard | null;
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
}

type Listener = (s: State) => void;

class Store {
  state: State = { project: null, templates: [], sequence: "", selection: null, track: null, clipboard: null, playhead: 0, playing: false, zoom: 60, tab: SEQUENCE_TAB, sequences: [], overlays: [], connected: false, jobs: {} };
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
    const [project, templates, seq] = await Promise.all([api.project(), api.templates(), api.sequence()]);
    this.set({ project, templates, sequence: seq.current, sequences: [seq.current] });
    subscribeEvents((e) => this.onEvent(e), (connected) => this.set({ connected }));
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
    } else if (e.type === "job") {
      const { type: _, ...job } = e;
      this.set({ jobs: { ...this.state.jobs, [e.id]: job } });
      if (e.done) toast(e.ok ? `${e.kind} done` : `${e.kind} failed: ${e.message}`, e.ok ? "ok" : "err", e.ok ? 4000 : 12000);
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
          const project = await api.project();
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
          this.set({ project, sequence, sequences, selection, track, overlays, tab });
        } while (this.refreshAgain);
      } catch (e) {
        toast(`Cannot reload the project: ${(e as Error).message}`, "err");
      } finally {
        this.refreshing = null;
      }
    })();
    return this.refreshing;
  }

  /** A selection survives a refresh only while the things it names still exist — each of
   *  them, so deleting one of three selected clips leaves the other two selected. */
  private stillThere(project: Project, sel: Selection): Selection {
    if (!sel) return null;
    if (sel.kind === "bin") return sel.id in project.bin ? sel : null;
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

  /** Point the inspector at a clip or a bin entry, and at nothing else. (`select` above is
   *  the subscription helper; this one is the selection, and they are deliberately not the
   *  same word.) */
  selectItem(kind: "clip" | "bin", id: string) {
    this.set({ selection: kind === "clip" ? { kind: "clip", ids: [id] } : { kind: "bin", id } });
  }

  /** Point the inspector at several clips at once — what a paste of several leaves behind. */
  selectClips(ids: string[]) {
    this.set({ selection: ids.length ? { kind: "clip", ids } : null });
  }

  /** Add a clip to the selection, or take it out again: Ctrl/Cmd- or Shift-click. The one
   *  just clicked goes last, so it is the one the inspector edits. */
  toggleClip(id: string) {
    const ids = this.selectedClips.filter((x) => x !== id);
    this.selectClips(ids.length === this.selectedClips.length ? [...ids, id] : ids);
  }

  /** Aim the next paste at a track. Clicking a track is the whole gesture: a paste needs
   *  a *where* as well as a *when*, and the playhead only answers the second half. */
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
      const err = e as Error & { status?: number; body?: { issues?: { message: string }[] } };
      if (err.status === 409) {
        toast("Project changed elsewhere — reloaded. Try again.", "err");
        await this.refresh();
      } else if (err.status === 422) {
        toast(`Rejected: ${(err.body?.issues ?? []).map((i) => i.message).join("; ") || err.message}`, "err", 8000);
      } else {
        toast(err.message, "err");
      }
      return false;
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

  /** JSON pointer to the open sequence's timeline — the prefix for every track edit. */
  get seqPath(): string {
    return seqPath(this.state.sequence);
  }

  /** Find a clip anywhere in the project; ids are unique across every sequence. Stops at
   *  the match rather than building the whole project's clip list: the preview calls this
   *  for every visible overlay on every frame. */
  findClip(id: string | null): { seq: string; track: Track; ti: number; clip: Clip; ci: number } | null {
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

  clipPath(id: string): string | null {
    const f = this.findClip(id);
    return f ? `${seqPath(f.seq)}/tracks/${f.ti}/clips/${f.ci}` : null;
  }

  /** A clip id free across the whole project: clip ids are global, not per sequence.
   *  `minted` is for a patch that adds several at once — those are not in the project
   *  yet, and two clips of one paste sharing an id would be refused. */
  newId(prefix: string, minted: Iterable<string> = []): string {
    return freeId(prefix, [...allClips(this.state.project!).map((c) => c.id), ...minted]);
  }

  /** A track id free across the whole project, for the same reason, `minted` included. */
  newTrackId(prefix: string, minted: Iterable<string> = []): string {
    const p = this.state.project!;
    return freeId(prefix, [...sequenceIds(p).flatMap((id) => sequenceOf(p, id)!.tracks.map((t) => t.id)), ...minted]);
  }

  /** Round to the frame grid. */
  snap(t: number): number {
    const f = this.fps;
    return Math.max(0, Math.round(t * f) / f);
  }

  /**
   * Jump to the nearest cut before or after the playhead — Alt+←/→. The cuts are every
   * clip start and end on the sequence on screen, whichever track they are on, plus 0, so
   * this walks the timeline the way an editor reads it: by where something changes.
   *
   * Nothing that way means staying put, rather than sliding to the end of a clip that is
   * not there. The half-frame margin keeps the playhead from picking the edge it is
   * already standing on, which `seek`'s rounding would put it back on.
   */
  seekEdge(dir: -1 | 1) {
    const p = this.state.playhead;
    const margin = 0.5 / this.fps;
    const edges = [0, ...this.tracks.flatMap((t) => t.clips.flatMap((c) => [c.start, c.end]))];
    const ahead = edges.filter((e) => (dir < 0 ? e < p - margin : e > p + margin));
    if (ahead.length) this.seek(dir < 0 ? Math.max(...ahead) : Math.min(...ahead));
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

/** `mm:ss.ff`, computed from whole frames so the frame field never reaches `fps`. */
export const fmtTime = (t: number, fps: number) => {
  const frames = Math.max(0, Math.round(t * fps));
  const whole = Math.floor(frames / fps);
  const fr = frames - Math.round(whole * fps);
  return `${String(Math.floor(whole / 60)).padStart(2, "0")}:${String(whole % 60).padStart(2, "0")}.${String(fr).padStart(2, "0")}`;
};
