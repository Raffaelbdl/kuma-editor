// Finding the two paths the shell cannot work without: the `kuma` binary it runs as a
// daemon, and the repository root that binary needs for `ui/dist`, `engine/build` and
// `node_modules/hyperframes`. Both follow the rules `kuma` itself documents, so the app
// and a hand-run `kuma serve` resolve the same things.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

export interface Kuma {
  /** Absolute path to the `kuma` executable, or the bare name if it was found on PATH. */
  bin: string;
  /** Repository root (`node_modules/hyperframes`, `ui/dist`, `engine/build`). */
  repo: string;
  /** `kuma --version`, once it has answered. */
  version: string;
}

/** Nearest ancestor of `from` (inclusive) for which `holds` is true. */
function ancestor(from: string, holds: (dir: string) => boolean): string | null {
  let dir = path.resolve(from);
  for (;;) {
    if (holds(dir)) return dir;
    const up = path.dirname(dir);
    if (up === dir) return null;
    dir = up;
  }
}

const isDir = (p: string) => {
  try {
    return fs.statSync(p).isDirectory();
  } catch {
    return false;
  }
};
const isFile = (p: string) => {
  try {
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
};

/**
 * The repository root, by the same rule the daemon uses: `KUMA_REPO`, else the nearest
 * ancestor holding `node_modules/hyperframes`. `appDir` is this shell's own directory,
 * which in a checkout is `<repo>/app`.
 */
export function findRepo(appDir: string): string | null {
  const env = process.env.KUMA_REPO;
  if (env && isDir(env)) return path.resolve(env);
  return ancestor(appDir, (d) => isDir(path.join(d, "node_modules", "hyperframes")));
}

/**
 * The `kuma` binary: `KUMA_BIN`, else the checkout's own build (release before debug, so
 * a stale debug build never shadows the one that was just released), else PATH.
 */
export function findBin(repo: string | null): string | null {
  const env = process.env.KUMA_BIN;
  if (env && isFile(env)) return path.resolve(env);
  if (repo) {
    for (const profile of ["release", "debug"]) {
      const candidate = path.join(repo, "target", profile, "kuma");
      if (isFile(candidate)) return candidate;
    }
  }
  return onPath("kuma");
}

function onPath(name: string): string | null {
  for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
    if (!dir) continue;
    const candidate = path.join(dir, name);
    if (isFile(candidate)) return candidate;
  }
  return null;
}

/**
 * Resolve both, or explain what is missing in the terms the README uses — this is the
 * error the user sees before anything else works, so it has to say what to run.
 */
export function resolveKuma(appDir: string): Kuma {
  const repo = findRepo(appDir);
  const bin = findBin(repo);
  if (!bin) {
    throw new Error(
      "Cannot find the `kuma` binary.\n\n" +
        "Build it with `cargo build --release` in the kuma checkout, install it with " +
        "`cargo install --path crates/kuma-cli`, or point KUMA_BIN at it.",
    );
  }
  if (!repo) {
    throw new Error(
      "Cannot find the kuma repository root (the directory holding `node_modules/hyperframes`).\n\n" +
        "Run `npm install` in the checkout, then set KUMA_REPO to it.",
    );
  }
  let version = "unknown";
  try {
    version = execFileSync(bin, ["--version"], { encoding: "utf8", timeout: 10_000 }).trim();
  } catch (e) {
    throw new Error(`\`${bin} --version\` failed: ${(e as Error).message}`);
  }
  return { bin, repo, version };
}

/** What `kuma doctor` would warn about, checked cheaply so the launcher can show it. */
export function repoWarnings(repo: string): string[] {
  const out: string[] = [];
  if (!isDir(path.join(repo, "ui", "dist"))) out.push("ui/dist is missing — run `npm run build` in ui/");
  if (!isFile(path.join(repo, "engine", "build", "kuma-preview"))) {
    out.push("the preview engine is not built — `cmake -S engine -B engine/build && cmake --build engine/build -j` (playback falls back to single frames without it)");
  }
  return out;
}
