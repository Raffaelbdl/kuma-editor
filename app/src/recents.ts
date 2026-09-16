// The recent-projects list behind the launcher.
//
// It is the shell's own state, not the project's: it lives in Electron's userData and
// never touches a `.kuma.json`. Entries carry a cached summary so the launcher can draw
// the list without starting a daemon for every project on it.

import fs from "node:fs";
import path from "node:path";

export interface Recent {
  /** Absolute path to the `*.kuma.json`. */
  path: string;
  name: string;
  /** ms since epoch, last time this shell opened it. */
  openedAt: number;
  width?: number;
  height?: number;
  fps?: string;
  /** Filled in when listing: the file is no longer where we left it. */
  missing?: boolean;
}

const MAX = 24;

export class Recents {
  private file: string;
  private items: Recent[] = [];

  constructor(userDataDir: string) {
    this.file = path.join(userDataDir, "recents.json");
    this.load();
  }

  private load() {
    try {
      const parsed: unknown = JSON.parse(fs.readFileSync(this.file, "utf8"));
      // A hand-edited or half-written file must not take the launcher down with it.
      this.items = Array.isArray(parsed) ? parsed.filter((x): x is Recent => typeof x?.path === "string") : [];
    } catch {
      this.items = [];
    }
  }

  private save() {
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true });
      fs.writeFileSync(this.file, JSON.stringify(this.items, null, 2));
    } catch (e) {
      console.error("kuma: cannot write the recents file:", e);
    }
  }

  /** Newest first, each marked with whether its file is still there. */
  list(): Recent[] {
    return this.items.map((r) => ({ ...r, ...summarize(r.path), missing: !fs.existsSync(r.path) }));
  }

  /** Record an open. Moves an existing entry to the front rather than duplicating it. */
  touch(file: string): void {
    const p = path.resolve(file);
    this.items = [{ path: p, name: nameOf(p), openedAt: Date.now(), ...summarize(p) }, ...this.items.filter((r) => path.resolve(r.path) !== p)].slice(0, MAX);
    this.save();
  }

  /** Forget one entry. Never deletes the project itself. */
  remove(file: string): void {
    const p = path.resolve(file);
    this.items = this.items.filter((r) => path.resolve(r.path) !== p);
    this.save();
  }

  clear(): void {
    this.items = [];
    this.save();
  }
}

/** Project name implied by `<name>.kuma.json`, matching `Project::name_from_path`. */
export function nameOf(file: string): string {
  return path.basename(file).replace(/\.kuma\.json$/i, "").replace(/\.json$/i, "");
}

/** Size and fps read straight off the file — a read-only peek, so no daemon is needed. */
function summarize(file: string): Partial<Recent> {
  try {
    const p = JSON.parse(fs.readFileSync(file, "utf8")) as { name?: string; width?: number; height?: number; fps?: { num?: number; den?: number } | number };
    const fps = typeof p.fps === "number" ? p.fps : p.fps?.num && p.fps?.den ? p.fps.num / p.fps.den : undefined;
    return {
      name: p.name || nameOf(file),
      width: p.width,
      height: p.height,
      // 29.97 rather than 29.970029970029973, and 30 rather than 30.00.
      fps: fps === undefined ? undefined : String(Math.round(fps * 100) / 100),
    };
  } catch {
    return {};
  }
}
