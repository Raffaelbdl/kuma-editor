import type { DaemonEvent, EffectPreset, Issue, Job, LintReport, PatchOp, Preset, Project, SequenceState, TemplateInfo } from "./types";

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

export const api = {
  project: () => json<Project>("/api/project"),
  patch: (patch: PatchOp[], base_revision: number, origin = "ui") =>
    json<{ revision: number; warnings: Issue[] }>("/api/project/patch", { method: "POST", body: JSON.stringify({ patch, base_revision, origin }) }),
  undo: () => json<{ revision: number | null }>("/api/project/undo", { method: "POST", body: "{}" }),
  redo: () => json<{ revision: number | null }>("/api/project/redo", { method: "POST", body: "{}" }),
  sequence: () => json<SequenceState>("/api/sequence"),
  openSequence: (sequence: string) =>
    json<{ current: string }>("/api/sequence", { method: "POST", body: JSON.stringify({ sequence }) }),
  templates: () => json<TemplateInfo[]>("/api/templates"),
  presets: () => json<Preset[]>("/api/presets"),
  effectPresets: () => json<EffectPreset[]>("/api/presets/effects"),
  thumbs: () => json<{ job: number }>("/api/templates/thumbs", { method: "POST", body: "{}" }),
  /** Check a template against the authoring contract. Reads only. */
  lintTemplate: (id: string) => json<LintReport>(`/api/templates/${encodeURIComponent(id)}/lint`),
  reloadTemplates: () => json<TemplateInfo[]>("/api/templates", { method: "POST", body: "{}" }),
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
  /** The jobs the daemon still remembers, oldest first. */
  jobs: (kind?: string) => json<{ jobs: Job[] }>(`/api/jobs${kind ? `?kind=${encodeURIComponent(kind)}` : ""}`),
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
