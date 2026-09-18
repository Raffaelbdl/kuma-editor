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

/** Run a command. Unknown ids and failures toast: a silent one here is invisible, which
 *  is how "+ new sequence…" came to do nothing at all. */
export function run(id: string, arg?: unknown): void {
  const c = registry.get(id);
  if (!c) {
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

/** Ids used in more than one place, so a typo is a compile error rather than a dead key. */
export const Cmd = {
  undo: "project.undo",
  redo: "project.redo",
  render: "project.render",
  newSequence: "project.new-sequence",
  newOverlay: "project.new-overlay",
  swapOrientation: "project.swap-orientation",
  transcribe: "project.transcribe",
  addMedia: "project.add-media",
  reloadPreview: "preview.reload",
  togglePlay: "preview.toggle-play",
  split: "timeline.split",
  deleteClip: "timeline.delete-clip",
  addTrack: "timeline.add-track",
  previewScale: "preview.scale",
  overlayTogglePlay: "overlay.toggle-play",
  overlayStep: "overlay.step",
} as const;
