import type { Activity, BlueprintInfo, LibraryInfo, HistoryEntry, MarkerInfo, CacheUsage, DaemonEvent, EffectPreset, FontFamily, Issue, Job, LintReport, OverlayInfo, PatchOp, Preset, Project, SequenceState } from "./types";

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
 *  would replace: a file there already, or unfinished renders to the same place. And how
 *  many frames it would build at once, which is the daemon's machine's to say. */
export interface RenderTarget { output: string; path: string; exists: boolean; pending: number[]; threads: number; max_threads: number }

/** What a render changes of its preset, each left to the preset (or the machine) when
 *  absent. The daemon refuses what the preset does not offer. */
export interface RenderOptions { threads?: number; quality?: number; speed?: string }

/** An export of text published beside a render: the text when nothing was written, else
 *  where it went (`path` project-relative, `output` absolute); and what to know about it. */
interface Exported { sequence: string; warnings: string[]; path?: string; output?: string }
export interface SubtitlesExport extends Exported { srt?: string; cues: number; entries: string[] }
export interface ChaptersExport extends Exported { text?: string; chapters: { start: number; at: string; title: string; clip?: string }[] }

export const api = {
  project: () => json<Project>("/api/project"),
  patch: (patch: PatchOp[], base_revision: number, origin = "ui") =>
    json<{ revision: number; warnings: Issue[] }>("/api/project/patch", { method: "POST", body: JSON.stringify({ patch, base_revision, origin }) }),
  /** Who asked is recorded beside the step, for the history panel. */
  undo: () => json<{ revision: number | null }>("/api/project/undo", { method: "POST", body: JSON.stringify({ origin: "ui" }) }),
  redo: () => json<{ revision: number | null }>("/api/project/redo", { method: "POST", body: JSON.stringify({ origin: "ui" }) }),
  /** What the agent has been doing, newest first: its tool calls over MCP. */
  activity: (limit = 50) => json<{ activity: Activity[] }>(`/api/activity?limit=${limit}`),
  /** The history, newest first, with the revision the document is at. */
  history: (limit = 200) => json<{ current: number; entries: HistoryEntry[] }>(`/api/history?limit=${limit}`),
  /** Make the document what it was at `revision`, as one new (undoable) edit. */
  restore: (revision: number) => json<{ revision: number }>("/api/history/restore", { method: "POST", body: JSON.stringify({ revision, origin: "ui" }) }),
  markers: () => json<{ markers: MarkerInfo[] }>("/api/markers"),
  /** Tick a marker answered by hand, or open it again. Session state, not an edit. */
  markerDone: (id: string, done: boolean) => json<unknown>("/api/markers/done", { method: "POST", body: JSON.stringify({ id, done }) }),
  sequence: () => json<SequenceState>("/api/sequence"),
  openSequence: (sequence: string) =>
    json<{ current: string }>("/api/sequence", { method: "POST", body: JSON.stringify({ sequence }) }),
  blueprints: () => json<BlueprintInfo[]>("/api/blueprints"),
  fonts: () => json<FontFamily[]>("/api/fonts"),
  /** Copy a font file (relative to the project, or absolute) into the project's `fonts/`,
   *  or the user library's when `shared`. */
  addFont: (path: string, shared: boolean) =>
    json<{ family: string; origin: string; path: string }>("/api/fonts", { method: "POST", body: JSON.stringify({ path, shared }) }),
  /** Rescan the libraries for blueprints. */
  reloadBlueprints: () => json<BlueprintInfo[]>("/api/blueprints", { method: "POST", body: "{}" }),
  /** None from a daemon older than the route: its blueprints are then shown unfoldered. */
  libraries: () => json<LibraryInfo[]>("/api/libraries").catch(() => [] as LibraryInfo[]),
  /** Read a library folder in every project on this machine. Nothing is copied. */
  addLibrary: (path: string) =>
    json<{ path: string; added: boolean; libraries: LibraryInfo[]; blueprints: BlueprintInfo[] }>("/api/libraries", { method: "POST", body: JSON.stringify({ path }) }),
  /** Stop reading an added library; its folder is left alone. */
  removeLibrary: (path: string) =>
    json<{ libraries: LibraryInfo[]; blueprints: BlueprintInfo[] }>("/api/libraries/remove", { method: "POST", body: JSON.stringify({ path }) }),
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
  thumbs: (which: { blueprints?: string[]; bins?: string[] }) => json<{ job: number }>("/api/blueprints/thumbs", { method: "POST", body: JSON.stringify({ ...which, origin: "ui: thumbs" }) }),
  /** What the timeline draws on a media entry's clips, made on first asking: its waveform,
   *  and the index of its filmstrip (`/api/bin/{id}/strip.jpg`). Looking, not an edit. */
  peaks: (bin: string) => json<{ rate: number; peaks: number[] }>(`/api/bin/${encodeURIComponent(bin)}/peaks.json`),
  strip: (bin: string) => json<{ strip: { count: number; step: number; width: number; height: number } }>(`/api/bin/${encodeURIComponent(bin)}/strip.json`),
  /** Check an overlay folder against the authoring contract. Reads only. */
  lintOverlay: (path: string) => json<LintReport>(`/api/overlays/lint?${new URLSearchParams({ path })}`),
  /** Add a media file, a folder's media files (`ids`, and what it passed over in
   *  `skipped`), or an overlay folder of the project (or its `overlay.json`). */
  /** One edit for every file. A folder is passed over (a person dropping one is not asking
   *  for all it holds), except an overlay folder on its own, which is imported. */
  addMedia: (paths: string[], base_revision: number, parent?: string | null) =>
    json<{ id: string | null; ids: string[]; skipped: { path: string; reason: string }[]; revision: number }>("/api/bin", {
      method: "POST",
      body: JSON.stringify({ ...(paths.length === 1 ? { path: paths[0] } : { paths }), files_only: true, base_revision, parent: parent ?? undefined, origin: "ui: add media" }),
    }),
  /** Probe media entries' files again and write what changed, as one edit. */
  reprobe: (bins: string[]) =>
    json<{ changed: { id: string; changes: Record<string, { from: unknown; to: unknown }> }[]; unchanged: string[]; failed: { id: string; reason: string }[]; revision: number }>(
      "/api/bin/reprobe", { method: "POST", body: JSON.stringify({ bins, origin: "ui: reload media" }) }),
  /** Speech to text. From a `bin` entry the result is added unplaced; from a `clip` it is
   *  placed over that clip, aligned. `replace` overwrites the transcript of an entry that
   *  already exists instead of refusing, keeping its styling and where it is placed. No
   *  `base_revision`: the model runs for minutes, and the daemon composes the patch
   *  against the project as it is when the words are in. */
  /** The whisper models installed, by file name; the first is the default. */
  whisperModels: () => json<{ whisper_models?: string[] }>("/api/health").then((h) => h.whisper_models ?? []),
  transcribe: (req: ({ bin: string } | { clip: string }) & { id?: string; language?: string; replace?: boolean; model?: string; snap?: boolean; split?: number }) =>
    json<{ job: number }>("/api/transcribe", { method: "POST", body: JSON.stringify({ ...req, origin: "ui: transcribe" }) }),
  /** Queue a render of `sequence`, or of the project's master when it is left out — the
   *  daemon's own default, so the two agree without the client having to know which is
   *  the master. `output` is project-relative; the daemon names one when it is left out. */
  render: (preset: string, sequence?: string, output?: string, options: RenderOptions = {}) =>
    json<{ job: number }>("/api/render", { method: "POST", body: JSON.stringify({ preset, sequence, output, ...options, origin: "ui: render" }) }),
  /** Where a render would write, whether a file is there already, and which unfinished
   *  renders will write the same path. Reads only. */
  renderTarget: (preset: string, sequence: string, output?: string) => {
    const q = new URLSearchParams({ preset, sequence });
    if (output) q.set("output", output);
    return json<RenderTarget>(`/api/render/target?${q}`);
  },
  /** `sequence`'s subtitles as SRT, in its own seconds: the daemon maps every cue through
   *  the clips placing it. With `write`, saved beside the renders. Reads only the document. */
  exportSubtitles: (sequence: string, opts: { write?: boolean; words_per_line?: number } = {}) =>
    json<SubtitlesExport>("/api/export/subtitles", { method: "POST", body: JSON.stringify({ sequence, ...opts }) }),
  /** `sequence`'s chapters as YouTube description timestamps, and why YouTube might not
   *  read them as chapters. Reads only. */
  exportChapters: (sequence: string) =>
    json<ChaptersExport>("/api/export/chapters", { method: "POST", body: JSON.stringify({ sequence }) }),
  /** The jobs the daemon still remembers, oldest first. */
  jobs: (kind?: string) => json<{ jobs: Job[] }>(`/api/jobs${kind ? `?kind=${encodeURIComponent(kind)}` : ""}`),
  /** Forget every finished job, for every window. Running and queued ones stay. */
  clearJobs: () => json<{ cleared: number[] }>("/api/jobs/clear", { method: "POST", body: "{}" }),
  /** Stop a queued or running render. It reports itself `cancelled` once it has. */
  cancelJob: (id: number) => json<Job>(`/api/jobs/${id}/cancel`, { method: "POST", body: "{}" }),
  cache: () => json<CacheUsage>("/api/cache"),
  /** A job: delete what `.kuma/` holds that the project no longer uses. */
  pruneCache: () => json<{ job: number }>("/api/cache/prune", { method: "POST", body: JSON.stringify({ origin: "ui: prune" }) }),
  /** Compile the open sequence for the engine; `force` hands it over even unchanged. Says
   *  what it did: which halves the engine took, and the nested overlays it baked or not. */
  previewReload: (opts: { scale?: number; force?: boolean } = {}) =>
    json<{ mlt: string; scale: number; changed: boolean; baked: number; baking: string[] }>("/api/preview/reload", { method: "POST", body: JSON.stringify(opts) }),
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
