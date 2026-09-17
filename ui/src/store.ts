import { api, subscribeEvents } from "./api";
import type { Clip, DaemonEvent, Job, PatchOp, Project, TemplateInfo, Track } from "./types";
import { allClips, fpsValue, freeId, isOverlaySource, seqDuration, seqPath, sequenceIds, sequenceOf } from "./types";

/**
 * What the inspector is showing. A clip and a bin entry are different things to inspect,
 * which is the model's own split: what is shown lives on the entry, when it is shown
 * lives on the clip. One selection addresses either, so the panel never has to guess.
 */
export type Selection = { kind: "clip"; id: string } | { kind: "bin"; id: string } | null;

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
  playhead: number;
  playing: boolean;
  /** pixels per second */
  zoom: number;
  /** Which editor the centre shows: `SEQUENCE_TAB` for the sequence monitor, else the bin
   *  id of an overlay entry open in the workbench. View state, like the selection. */
  tab: string;
  /** Overlay entries open as tabs, oldest first. */
  overlays: string[];
  connected: boolean;
  jobs: Record<number, Job>;
}

type Listener = (s: State) => void;

class Store {
  state: State = { project: null, templates: [], sequence: "", selection: null, playhead: 0, playing: false, zoom: 60, tab: SEQUENCE_TAB, overlays: [], connected: false, jobs: {} };
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
    this.set({ project, templates, sequence: seq.current });
    subscribeEvents((e) => this.onEvent(e), (connected) => this.set({ connected }));
  }

  /** Open a sequence for editing. The daemon recompiles the preview for it, so this is a
   *  round trip rather than a local flag. */
  async openSequence(id: string) {
    if (id === this.state.sequence) return;
    try {
      const { current } = await api.openSequence(id);
      // Opening a timeline is asking to look at it: the monitor comes forward with it.
      this.set({ sequence: current, selection: null, playhead: 0, tab: SEQUENCE_TAB });
    } catch (e) {
      toast((e as Error).message, "err");
    }
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
          // An entry open in the workbench can be removed out from under us (undo, an
          // agent edit) exactly as the open sequence can: its tab goes with it.
          const overlays = this.state.overlays.filter((id) => isOverlayEntry(project, id));
          const tab = overlays.includes(this.state.tab) ? this.state.tab : SEQUENCE_TAB;
          this.set({ project, sequence, selection, overlays, tab });
        } while (this.refreshAgain);
      } catch (e) {
        toast(`Cannot reload the project: ${(e as Error).message}`, "err");
      } finally {
        this.refreshing = null;
      }
    })();
    return this.refreshing;
  }

  /** A selection survives a refresh only while the thing it names still exists. */
  private stillThere(project: Project, sel: Selection): Selection {
    if (!sel) return null;
    if (sel.kind === "bin") return sel.id in project.bin ? sel : null;
    return allClips(project).some((c) => c.id === sel.id) ? sel : null;
  }

  /**
   * Open an overlay entry in the workbench and bring its tab forward. Selecting it too is
   * the point rather than a courtesy: the inspector is where a bin entry's vars are
   * edited, and the workbench deliberately does not grow a second copy of them.
   */
  openOverlay(id: string) {
    if (!isOverlayEntry(this.state.project, id)) {
      toast(`${id} is not an overlay entry`, "err");
      return;
    }
    const overlays = this.state.overlays.includes(id) ? this.state.overlays : [...this.state.overlays, id];
    this.set({ overlays, tab: id, selection: { kind: "bin", id } });
  }

  /** Close a workbench tab. Only the tab: the entry stays in the bin, where it is the
   *  thing every clip plays. */
  closeOverlay(id: string) {
    const overlays = this.state.overlays.filter((x) => x !== id);
    const tab = this.state.tab === id ? overlays.at(-1) ?? SEQUENCE_TAB : this.state.tab;
    this.set({ overlays, tab });
  }

  /** Show one of the open tabs (`SEQUENCE_TAB` for the sequence monitor). */
  showTab(tab: string) {
    if (tab !== SEQUENCE_TAB && !this.state.overlays.includes(tab)) return;
    this.set({ tab, ...(tab === SEQUENCE_TAB ? {} : { selection: { kind: "bin", id: tab } as Selection }) });
  }

  /** Point the inspector at a clip or a bin entry. (`select` above is the subscription
   *  helper; this one is the selection, and they are deliberately not the same word.) */
  selectItem(kind: "clip" | "bin", id: string) {
    this.set({ selection: { kind, id } as Selection });
  }

  /** The selected clip, or null when a bin entry (or nothing) is selected. */
  get selectedClip(): string | null {
    return this.state.selection?.kind === "clip" ? this.state.selection.id : null;
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

  /** A clip id free across the whole project: clip ids are global, not per sequence. */
  newId(prefix: string): string {
    return freeId(prefix, allClips(this.state.project!).map((c) => c.id));
  }

  /** A track id free across the whole project, for the same reason. */
  newTrackId(prefix: string): string {
    const p = this.state.project!;
    return freeId(prefix, sequenceIds(p).flatMap((id) => sequenceOf(p, id)!.tracks.map((t) => t.id)));
  }

  /** Round to the frame grid. */
  snap(t: number): number {
    const f = this.fps;
    return Math.max(0, Math.round(t * f) / f);
  }

  /** Move the playhead; it may sit past the last clip (that is where new clips go). */
  seek(t: number) {
    this.set({ playhead: this.snap(Math.min(Math.max(0, t), this.duration + 10)) });
  }
}

export const store = new Store();

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
