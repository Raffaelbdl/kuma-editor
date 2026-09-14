import { api, subscribeEvents } from "./api";
import type { Clip, DaemonEvent, PatchOp, Project, TemplateInfo, Track } from "./types";
import { fpsValue, freeId } from "./types";

export interface State {
  project: Project | null;
  templates: TemplateInfo[];
  selection: string | null;
  playhead: number;
  playing: boolean;
  /** pixels per second */
  zoom: number;
  connected: boolean;
  jobs: Record<number, { kind: string; percent: number; message: string; done?: boolean; ok?: boolean; result?: unknown }>;
}

type Listener = (s: State) => void;

class Store {
  state: State = { project: null, templates: [], selection: null, playhead: 0, playing: false, zoom: 60, connected: false, jobs: {} };
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
    const [project, templates] = await Promise.all([api.project(), api.templates()]);
    this.set({ project, templates });
    subscribeEvents((e) => this.onEvent(e), (connected) => this.set({ connected }));
  }

  private async onEvent(e: DaemonEvent) {
    if (e.type === "project_changed") {
      if (!this.state.project || e.revision !== this.state.project.revision) await this.refresh();
      toast(`${e.origin} → r${e.revision}`, "ok", 1200);
    } else if (e.type === "job_progress") {
      this.set({ jobs: { ...this.state.jobs, [e.id]: { kind: e.kind, percent: e.percent, message: e.message } } });
    } else if (e.type === "job_done") {
      this.set({ jobs: { ...this.state.jobs, [e.id]: { kind: e.kind, percent: 100, message: e.message, done: true, ok: e.ok, result: e.result } } });
      toast(e.ok ? `${e.kind} done` : `${e.kind} failed: ${e.message}`, e.ok ? "ok" : "err", e.ok ? 4000 : 12000);
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
          const selection = project.tracks.some((t) => t.clips.some((c) => c.id === this.state.selection)) ? this.state.selection : null;
          this.set({ project, selection });
        } while (this.refreshAgain);
      } catch (e) {
        toast(`Cannot reload the project: ${(e as Error).message}`, "err");
      } finally {
        this.refreshing = null;
      }
    })();
    return this.refreshing;
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

  get duration(): number {
    const p = this.state.project;
    if (!p) return 0;
    return Math.max(0, ...p.tracks.flatMap((t) => t.clips.map((c) => c.end)));
  }

  findClip(id: string | null): { track: Track; ti: number; clip: Clip; ci: number } | null {
    const p = this.state.project;
    if (!p || !id) return null;
    for (let ti = 0; ti < p.tracks.length; ti++) {
      const ci = p.tracks[ti].clips.findIndex((c) => c.id === id);
      if (ci >= 0) return { track: p.tracks[ti], ti, clip: p.tracks[ti].clips[ci], ci };
    }
    return null;
  }

  clipPath(id: string): string | null {
    const f = this.findClip(id);
    return f ? `/tracks/${f.ti}/clips/${f.ci}` : null;
  }

  /** A clip id that is free across all tracks (clip ids are global). */
  newId(prefix: string): string {
    return freeId(prefix, this.state.project!.tracks.flatMap((t) => t.clips.map((c) => c.id)));
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

// ---- toasts ------------------------------------------------------------------

export function toast(text: string, kind: "ok" | "err" | "" = "", ms = 3500) {
  const host = document.getElementById("toasts")!;
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = text;
  host.appendChild(el);
  setTimeout(() => el.remove(), ms);
}

/** `mm:ss.ff`, computed from whole frames so the frame field never reaches `fps`. */
export const fmtTime = (t: number, fps: number) => {
  const frames = Math.max(0, Math.round(t * fps));
  const whole = Math.floor(frames / fps);
  const fr = frames - Math.round(whole * fps);
  return `${String(Math.floor(whole / 60)).padStart(2, "0")}:${String(whole % 60).padStart(2, "0")}.${String(fr).padStart(2, "0")}`;
};
