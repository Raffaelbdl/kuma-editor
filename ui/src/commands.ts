// The editor's actions, in one place.
//
// Kdenlive keeps its actions in a collection and lets the menus, the toolbars and the
// shortcuts all refer to the same entry. Here the same three surfaces existed — a toolbar
// button, an item in the desktop application menu, and a key — and each wired the action
// its own way, so adding one meant touching three files and the `Preview` instance had to
// be handed down to whichever function needed it.
//
// Now a panel registers what it can do when it mounts, and every surface looks it up by
// id. Nothing has to hold a reference to the panel that does the work.

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

/**
 * Run a command by id. Unknown ids and failures become a toast rather than an unhandled
 * rejection — this is the path menus and keystrokes take, and a silent one is how
 * "+ new sequence…" managed to do nothing for a whole release.
 */
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
  addMedia: "project.add-media",
  reloadPreview: "preview.reload",
  togglePlay: "preview.toggle-play",
  split: "timeline.split",
  deleteClip: "timeline.delete-clip",
  addTrack: "timeline.add-track",
  previewScale: "preview.scale",
} as const;
