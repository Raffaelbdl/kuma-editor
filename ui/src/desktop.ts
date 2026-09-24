// Desktop shell integration.
//
// The editor is the same page whether it is opened in a browser or in the kuma app, so
// everything here is behind one check: the app's preload puts `kumaApp` on `window`, and
// a browser does not. `desktop()` returns null there and every caller degrades to what
// the browser already did.

import { api } from "./api";
import { Cmd, run } from "./commands";
import { store, toast } from "./store";

export interface DesktopBridge {
  version: string;
  projectFile: string;
  projectDir: string;
  daemonUrl: string;
  /** False when the app attached to a daemon it did not start (and so will not stop it). */
  ownsDaemon: boolean;
  pickMedia(): Promise<string[]>;
  /** Where a dropped file is on disk, or "". */
  pathForFile(file: File): string;
  reveal(relativePath?: string): Promise<string>;
  showLauncher(): Promise<void>;
  onCommand(fn: (name: string) => void): () => void;
  /** Top-level application-menu names, for the window's own title bar. */
  menuLabels(): Promise<string[]>;
  /** Drop the real submenu under `label`, at viewport coordinates. Resolves when it closes. */
  popupMenu(label: string, x: number, y: number): Promise<boolean>;
  onMenuChanged(fn: (labels: string[]) => void): () => void;
  windowControl(action: "minimize" | "maximize" | "close"): Promise<{ maximized: boolean } | null>;
  windowState(): Promise<{ maximized: boolean; fullScreen: boolean }>;
  onWindowState(fn: (s: { maximized: boolean; fullScreen: boolean }) => void): () => void;
}

/** The bridge, or null when this is an ordinary browser tab. */
export function desktop(): DesktopBridge | null {
  return (globalThis as { kumaApp?: DesktopBridge }).kumaApp ?? null;
}

/** What the application menu can send (`app/src/main.ts`), by the command it runs. A
 *  menu action is one line here; the registry in `commands.ts` holds the behaviour. */
const MENU: Record<string, string> = {
  "undo": Cmd.undo,
  "redo": Cmd.redo,
  "render": Cmd.render,
  "reload-preview": Cmd.reloadPreview,
  "new-sequence": Cmd.newSequence,
  "new-overlay": Cmd.newOverlay,
  "swap-orientation": Cmd.swapOrientation,
  "add-media": Cmd.addMedia,
  "copy-clip": Cmd.copyClip,
  "cut-clip": Cmd.cutClip,
  "paste-clip": Cmd.pasteClip,
  shortcuts: Cmd.shortcuts,
  theme: Cmd.theme,
  "group-clips": Cmd.group,
  "ungroup-clips": Cmd.ungroup,
};

export function initDesktop(): void {
  desktop()?.onCommand((name) => { if (MENU[name]) run(MENU[name]); });
}

/**
 * Add media through the desktop file picker. Paths come back relative to the project
 * root where possible, which is what keeps a project movable.
 */
export async function pickAndAddMedia(): Promise<void> {
  const app = desktop();
  if (!app) return;
  let paths: string[];
  try {
    paths = await app.pickMedia();
  } catch (e) {
    toast(`Cannot open the file picker: ${(e as Error).message}`, "err");
    return;
  }
  await addMediaPaths(paths, null);
}

/** Add files dropped from the desktop, under `parent` if dropped on a folder. Only the
 *  shell can say where a dropped file is; a drop can also land empty (see `isFileDrag`). */
export async function dropMedia(files: FileList, parent: string | null): Promise<void> {
  const app = desktop();
  if (!files.length) {
    toast("The drop arrived without its files. Drag it again, or use Browse…", "err", 8000);
    return;
  }
  if (!app) {
    toast("A browser cannot add dropped files. Type the path below, or use the desktop app.", "err", 8000);
    return;
  }
  await addMediaPaths([...files].map((f) => app.pathForFile(f)).filter(Boolean), parent);
}

/** Add each path to the bin, stopping at the first that fails. An `overlay.json` is
 *  imported as an overlay; anything else is media. */
async function addMediaPaths(paths: string[], parent: string | null): Promise<void> {
  if (!paths.length) return;
  // One at a time, against the revision each previous add produced: the daemon refuses a
  // patch built on a stale revision, and a multi-select would hit that on the second file.
  const added: string[] = [];
  for (const path of paths) {
    const project = store.state.project;
    if (!project) return;
    try {
      const add = /(^|[\\/])overlay\.json$/.test(path) ? api.importOverlay : api.addMedia;
      const r = await add(path, project.revision, parent);
      added.push(r.id);
      await store.refresh();
    } catch (e) {
      toast(`${path}: ${(e as Error).message}`, "err", 8000);
      break;
    }
  }
  if (added.length) toast(`Added ${added.join(", ")} to the bin`, "ok");
}
