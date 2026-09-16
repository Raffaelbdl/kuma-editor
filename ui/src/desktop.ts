// Desktop shell integration.
//
// The editor is the same page whether it is opened in a browser or in the kuma app, so
// everything here is behind one check: the app's preload puts `kumaApp` on `window`, and
// a browser does not. `desktop()` returns null there and every caller degrades to what
// the browser already did.

import { api } from "./api";
import { store, toast } from "./store";

export interface DesktopBridge {
  version: string;
  projectFile: string;
  projectDir: string;
  daemonUrl: string;
  /** False when the app attached to a daemon it did not start (and so will not stop it). */
  ownsDaemon: boolean;
  pickMedia(): Promise<string[]>;
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

/** Commands the application menu can send. Each is whatever the toolbar button does. */
export interface Commands {
  undo(): void;
  redo(): void;
  render(): void;
  reloadPreview(): void;
  newSequence(): void;
  swapOrientation(): void;
  addMedia(): void;
}

export function initDesktop(commands: Commands): void {
  const app = desktop();
  if (!app) return;
  app.onCommand((name) => {
    switch (name) {
      case "undo": return commands.undo();
      case "redo": return commands.redo();
      case "render": return commands.render();
      case "reload-preview": return commands.reloadPreview();
      case "new-sequence": return commands.newSequence();
      case "swap-orientation": return commands.swapOrientation();
      case "add-media": return commands.addMedia();
    }
  });
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
  if (!paths.length) return;
  // One at a time, against the revision each previous add produced: the daemon refuses a
  // patch built on a stale revision, and a multi-select would hit that on the second file.
  const added: string[] = [];
  for (const path of paths) {
    const project = store.state.project;
    if (!project) return;
    try {
      const r = await api.addMedia(path, project.revision);
      added.push(r.id);
      await store.refresh();
    } catch (e) {
      toast(`${path}: ${(e as Error).message}`, "err", 8000);
      break;
    }
  }
  if (added.length) toast(`Added ${added.join(", ")} to the bin`, "ok");
}
