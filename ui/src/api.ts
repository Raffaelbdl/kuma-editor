import type { DaemonEvent, Issue, PatchOp, Preset, Project, TemplateInfo } from "./types";

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
  health: () => json<Record<string, unknown>>("/api/health"),
  project: () => json<Project>("/api/project"),
  patch: (patch: PatchOp[], base_revision: number, origin = "ui") =>
    json<{ revision: number; warnings: Issue[] }>("/api/project/patch", { method: "POST", body: JSON.stringify({ patch, base_revision, origin }) }),
  undo: () => json<{ revision: number | null }>("/api/project/undo", { method: "POST", body: "{}" }),
  redo: () => json<{ revision: number | null }>("/api/project/redo", { method: "POST", body: "{}" }),
  history: () => json<{ undo: string[]; redo: string[] }>("/api/project/history"),
  validate: () => json<{ issues: Issue[]; duration: number }>("/api/project/validate"),
  templates: () => json<TemplateInfo[]>("/api/templates"),
  presets: () => json<Preset[]>("/api/presets"),
  addAsset: (path: string, base_revision: number) =>
    json<{ id: string; revision: number }>("/api/assets", { method: "POST", body: JSON.stringify({ path, base_revision, origin: "ui: add asset" }) }),
  render: (preset: string, supersample = 1) => json<{ job: number }>("/api/render", { method: "POST", body: JSON.stringify({ preset, supersample }) }),
  bake: (clips?: string[]) => json<{ job: number }>("/api/bake", { method: "POST", body: JSON.stringify({ clips }) }),
  previewReload: () => json<{ mlt: string }>("/api/preview/reload", { method: "POST", body: "{}" }),
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
