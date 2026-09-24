import type { BlueprintInfo, DaemonEvent, EffectPreset, Issue, Job, LintReport, OverlayInfo, PatchOp, Preset, Project, SequenceState } from "./types";

async function json<T>(url: string, init?: RequestInit): Promise<T> {
  const r = await fetch(url, { headers: { "content-type": "application/json" }, ...init });
  const body = await r.json().catch(() => ({}));
  if (!r.ok) {
    const err = new Error(body.error ?? `${r.status} ${r.statusText}`) as Error & { status: number; body: unknown };
    err.status = r.status;
    err.body = body;
    throw err;
  }
  return body as T;
}

/** Where a render would write (`path` project-relative when it can be) and what it
 *  would replace: a file there already, or unfinished renders to the same place. */
export interface RenderTarget { output: string; path: string; exists: boolean; pending: number[] }

export const api = {
  project: () => json<Project>("/api/project"),
  patch: (patch: PatchOp[], base_revision: number, origin = "ui") =>
    json<{ revision: number; warnings: Issue[] }>("/api/project/patch", { method: "POST", body: JSON.stringify({ patch, base_revision, origin }) }),
  undo: () => json<{ revision: number | null }>("/api/project/undo", { method: "POST", body: "{}" }),
  redo: () => json<{ revision: number | null }>("/api/project/redo", { method: "POST", body: "{}" }),
  sequence: () => json<SequenceState>("/api/sequence"),
  openSequence: (sequence: string) =>
    json<{ current: string }>("/api/sequence", { method: "POST", body: JSON.stringify({ sequence }) }),
  blueprints: () => json<BlueprintInfo[]>("/api/blueprints"),
  /** Rescan the libraries for blueprints. */
  reloadBlueprints: () => json<BlueprintInfo[]>("/api/blueprints", { method: "POST", body: "{}" }),
  /** Copy a blueprint into `overlays/`; not an edit. Returns the folder. */
  copyBlueprint: (id: string, folder?: string) =>
    json<{ overlay: string }>(`/api/blueprints/${encodeURIComponent(id)}/copy`, { method: "POST", body: JSON.stringify({ folder }) }),
  /** The manifest of every overlay folder the bin uses, by folder. */
  overlays: () => json<Record<string, OverlayInfo>>("/api/overlays"),
  /** Give an overlay entry its own copy of the folder it shares. One edit. */
  makeUnique: (bin: string) =>
    json<{ overlay: string; revision: number }>("/api/overlays/unique", { method: "POST", body: JSON.stringify({ bin, origin: "ui: make unique" }) }),
  /** Save an overlay entry as a blueprint, its vars as defaults. */
  saveBlueprint: (bin: string, id: string, force = false) =>
    json<{ dir: string }>("/api/overlays/save-blueprint", { method: "POST", body: JSON.stringify({ bin, id, force }) }),
  presets: () => json<Preset[]>("/api/presets"),
  effectPresets: () => json<EffectPreset[]>("/api/presets/effects"),
  /** Render previews of the given blueprints and overlay entries. */
  thumbs: (which: { blueprints?: string[]; bins?: string[] }) => json<{ job: number }>("/api/blueprints/thumbs", { method: "POST", body: JSON.stringify(which) }),
  /** Check an overlay folder against the authoring contract. Reads only. */
  lintOverlay: (path: string) => json<LintReport>(`/api/overlays/lint?${new URLSearchParams({ path })}`),
  /** Add a media file, or an overlay folder of the project (or its `overlay.json`). */
  addMedia: (path: string, base_revision: number, parent?: string | null) =>
    json<{ id: string; revision: number }>("/api/bin", { method: "POST", body: JSON.stringify({ path, base_revision, parent: parent ?? undefined, origin: "ui: add media" }) }),
  /** Speech to text. From a `bin` entry the result is added unplaced; from a `clip` it is
   *  placed over that clip, aligned. `replace` overwrites the transcript of an entry that
   *  already exists instead of refusing, keeping its styling and where it is placed. No
   *  `base_revision`: the model runs for minutes, and the daemon composes the patch
   *  against the project as it is when the words are in. */
  transcribe: (req: ({ bin: string } | { clip: string }) & { id?: string; language?: string; replace?: boolean }) =>
    json<{ job: number }>("/api/transcribe", { method: "POST", body: JSON.stringify({ ...req, origin: "ui: transcribe" }) }),
  /** Queue a render of `sequence`, or of the project's master when it is left out — the
   *  daemon's own default, so the two agree without the client having to know which is
   *  the master. `output` is project-relative; the daemon names one when it is left out. */
  render: (preset: string, sequence?: string, output?: string) =>
    json<{ job: number }>("/api/render", { method: "POST", body: JSON.stringify({ preset, sequence, output }) }),
  /** Where a render would write, whether a file is there already, and which unfinished
   *  renders will write the same path. Reads only. */
  renderTarget: (preset: string, sequence: string, output?: string) => {
    const q = new URLSearchParams({ preset, sequence });
    if (output) q.set("output", output);
    return json<RenderTarget>(`/api/render/target?${q}`);
  },
  /** The jobs the daemon still remembers, oldest first. */
  jobs: (kind?: string) => json<{ jobs: Job[] }>(`/api/jobs${kind ? `?kind=${encodeURIComponent(kind)}` : ""}`),
  /** Forget every finished job, for every window. Running and queued ones stay. */
  clearJobs: () => json<{ cleared: number[] }>("/api/jobs/clear", { method: "POST", body: "{}" }),
  /** Stop a queued or running render. It reports itself `cancelled` once it has. */
  cancelJob: (id: number) => json<Job>(`/api/jobs/${id}/cancel`, { method: "POST", body: "{}" }),
  previewReload: (scale?: number) => json<{ mlt: string; scale: number }>("/api/preview/reload", { method: "POST", body: JSON.stringify(scale === undefined ? {} : { scale }) }),
  /** The live document for an overlay **bin entry**, at the length and in the frame the
   *  workbench is looking at. No clip: an entry is editable before anything places it.
   *  `rev` only defeats the browser cache when the iframe is recreated. */
  overlayEntryUrl: (id: string, duration: number, width: number, height: number, rev: number) =>
    `/overlay-entry/${encodeURIComponent(id)}?duration=${duration.toFixed(3)}&width=${width}&height=${height}&rev=${rev}`,
  snapshotUrl: (t: number, width = 960) => `/api/snapshot?t=${t.toFixed(3)}&width=${width}&_=${Date.now()}`,
};

/** Subscribe to daemon events with automatic reconnect. */
export function subscribeEvents(onEvent: (e: DaemonEvent) => void, onStatus?: (connected: boolean) => void): () => void {
  let ws: WebSocket | null = null;
  let closed = false;
  let timer = 0;
  const connect = () => {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    ws = new WebSocket(`${proto}://${location.host}/api/events`);
    ws.onopen = () => onStatus?.(true);
    ws.onmessage = (m) => {
      try { onEvent(JSON.parse(m.data)); } catch { /* ignore */ }
    };
    ws.onclose = () => {
      onStatus?.(false);
      if (!closed) timer = window.setTimeout(connect, 1500);
    };
    ws.onerror = () => ws?.close();
  };
  connect();
  return () => { closed = true; clearTimeout(timer); ws?.close(); };
}
