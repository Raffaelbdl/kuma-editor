// The editor's actions, in one place — Kdenlive's action collection.
//
// A panel registers what it can do when it mounts; the toolbar, the keyboard and the
// desktop application menu each reach those by id. So an action is written once, and
// nothing has to hold a reference to the panel that performs it.

import { toast } from "./store";

export interface Command {
  id: string;
  /** What a menu or a button would call it. */
  label: string;
  run(arg?: unknown): unknown;
}

const registry = new Map<string, Command>();

/** Register a command, replacing any earlier one with the same id (a panel remounting). */
export function command(id: string, label: string, run: (arg?: unknown) => unknown): void {
  registry.set(id, { id, label, run });
}

export function has(id: string): boolean {
  return registry.has(id);
}

export function label(id: string): string {
  return registry.get(id)?.label ?? id;
}

/** Every command, in the order registered: what the ⌘K palette offers. */
export function list(): Command[] {
  return [...registry.values()];
}

/** Where a command this page does not have goes instead: a panel in a window of its own
 *  hands the transport's and the preview's to the editor window (`viewsync.ts`). */
let forward: ((id: string, arg?: unknown) => boolean) | null = null;
export function forwardUnknown(fn: typeof forward): void {
  forward = fn;
}

/** Run a command. Unknown ids and failures toast: a silent one here is invisible, which
 *  is how "+ new sequence…" came to do nothing at all. */
export function run(id: string, arg?: unknown): void {
  const c = registry.get(id);
  if (!c) {
    if (forward?.(id, arg)) return;
    console.error(`no such command: ${id}`);
    toast(`Nothing is registered for "${id}"`, "err");
    return;
  }
  try {
    const r = c.run(arg);
    if (r instanceof Promise) r.catch((e: unknown) => toast((e as Error).message, "err"));
  } catch (e) {
    toast((e as Error).message, "err");
  }
}

/** What a step command takes: frames of the surface's own rate, or seconds. */
export type Step = { frames: number } | { seconds: number };

/** `by` in seconds, at `fps` frames a second. */
export const stepSeconds = (by: Step, fps: number) => ("frames" in by ? by.frames / fps : by.seconds);

/** Ids used in more than one place, so a typo is a compile error rather than a dead key. */
export const Cmd = {
  undo: "project.undo",
  redo: "project.redo",
  render: "project.render",
  queue: "project.queue",
  newSequence: "project.new-sequence",
  newOverlay: "project.new-overlay",
  newTitle: "project.new-title",
  swapOrientation: "project.swap-orientation",
  transcribe: "project.transcribe",
  addMedia: "project.add-media",
  reloadPreview: "preview.reload",
  togglePlay: "preview.toggle-play",
  split: "timeline.split",
  seekEdge: "timeline.seek-edge",
  deleteClip: "timeline.delete-clip",
  addMarker: "markers.add",
  autoKey: "timeline.auto-key",
  shortcuts: "help.shortcuts",
  theme: "view.theme",
  group: "timeline.group",
  ungroup: "timeline.ungroup",
  copyClip: "timeline.copy-clip",
  cutClip: "timeline.cut-clip",
  pasteClip: "timeline.paste-clip",
  addTrack: "timeline.add-track",
  previewScale: "preview.scale",
  overlayTogglePlay: "overlay.toggle-play",
  overlayStep: "overlay.step",
  viewerTogglePlay: "viewer.toggle-play",
  viewerStep: "viewer.step",
  palette: "view.palette",
  modeDen: "view.den",
  modePower: "view.power",
  toggleMode: "view.toggle-mode",
  resetDock: "view.reset-dock",
  movablePanels: "view.movable-panels",
} as const;
