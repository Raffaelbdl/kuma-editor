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

/** The repository root, by the daemon's own rule: `KUMA_REPO`, else the nearest ancestor
 *  holding `node_modules/hyperframes`. `appDir` is `<repo>/app` in a checkout. */
export function findRepo(appDir: string): string | null {
  const env = process.env.KUMA_REPO;
  if (env && isDir(env)) return path.resolve(env);
  return ancestor(appDir, (d) => isDir(path.join(d, "node_modules", "hyperframes")));
}

/** The `kuma` binary: `KUMA_BIN`, else the checkout's build (release before debug, so a
 *  stale debug build never shadows a fresh release), else PATH. */
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

/** Resolve both, or say what to run — this is the error seen before anything works. */
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

/** Whether the binary predates the checkout. `build.rs` stamps the commit into
 *  `--version` for exactly this, and a binary older than the bin refactor writes projects
 *  nothing here can open. Only a strict ancestor counts, so another branch, a non-git
 *  install or a newer commit stays quiet rather than crying wolf. */
function staleBinary(repo: string, version: string): string | null {
  const stamped = /\(([0-9a-f]{7,40})\)/.exec(version)?.[1];
  if (!stamped) return null;
  const git = (args: string[]) => {
    try {
      return execFileSync("git", args, { cwd: repo, encoding: "utf8", timeout: 5_000, stdio: ["ignore", "pipe", "ignore"] }).trim();
    } catch {
      return null;
    }
  };
  // Exits 0 only when `stamped` is a commit this repo has and HEAD can reach, which also
  // covers "not a git checkout" and "commit we have never heard of".
  try {
    execFileSync("git", ["merge-base", "--is-ancestor", stamped, "HEAD"], { cwd: repo, timeout: 5_000, stdio: "ignore" });
  } catch {
    return null;
  }
  const behind = git(["rev-list", "--count", `${stamped}..HEAD`]);
  if (!behind || behind === "0") return null;
  return `the kuma binary is ${behind} commit${behind === "1" ? "" : "s"} behind this checkout — \`cargo build --release\` (an old binary writes projects this version cannot open)`;
}

/** The user library `kuma` itself would read, by the rule the README documents. */
function userLibraryDir(): string | null {
  const base = process.env.XDG_DATA_HOME || (process.env.HOME ? path.join(process.env.HOME, ".local", "share") : null);
  return base ? path.join(base, "kuma") : null;
}

/**
 * `KUMA_LIBRARY` to hand the daemon, or null to leave it alone.
 *
 * With no library installed a daemon loads zero templates, and one that *is* found
 * renders without `_vendor/kuma.css` — blank, which reads as a broken template. The
 * checkout ships the starter set, so point at it.
 *
 * Only as a fallback: `KUMA_LIBRARY` outranks the user library, so setting it always
 * would shadow templates they had installed and edited, and only inside the app.
 */
export function libraryFallback(repo: string): string | null {
  if (process.env.KUMA_LIBRARY) return null;
  const user = userLibraryDir();
  if (user && isDir(path.join(user, "overlays"))) {
    try {
      if (fs.readdirSync(path.join(user, "overlays")).length) return null;
    } catch {
      return null;
    }
  }
  const shipped = path.join(repo, "library");
  return isDir(path.join(shipped, "overlays")) ? shipped : null;
}

/** Newest mtime under `root`, or 0 if it does not exist. Directories are walked. */
function newestMtime(root: string, depth = 6): number {
  let stat;
  try {
    stat = fs.statSync(root);
  } catch {
    return 0;
  }
  if (!stat.isDirectory()) return stat.mtimeMs;
  if (depth <= 0) return stat.mtimeMs;
  let newest = 0;
  for (const e of fs.readdirSync(root)) {
    newest = Math.max(newest, newestMtime(path.join(root, e), depth - 1));
  }
  return newest;
}

/**
 * Whether `ui/dist` predates the sources it is built from.
 *
 * The daemon serves `ui/dist` and it is not in git, so pulling a branch updates `ui/src`
 * and leaves the build behind. The editor window then loads an old bundle against a new
 * project and fails on the document's shape — which looks like a broken project, not a
 * missing build step. mtimes are enough: a checkout touches the files it updates.
 */
export function uiBuildProblem(repo: string): string | null {
  const dist = newestMtime(path.join(repo, "ui", "dist"));
  if (!dist) return "ui/dist is missing — run `npm run build` in ui/";
  const src = Math.max(
    newestMtime(path.join(repo, "ui", "src")),
    newestMtime(path.join(repo, "ui", "index.html")),
    newestMtime(path.join(repo, "ui", "vite.config.ts")),
    newestMtime(path.join(repo, "ui", "package.json")),
  );
  return src > dist ? "ui/dist is older than ui/src — `npm run build` in ui/ (the editor will fail to start against a newer project)" : null;
}

/** What `kuma doctor` would warn about, checked cheaply so the launcher can show it. */
export function repoWarnings(repo: string, version?: string): string[] {
  const out: string[] = [];
  if (version) {
    const stale = staleBinary(repo, version);
    if (stale) out.push(stale);
  }
  const ui = uiBuildProblem(repo);
  if (ui) out.push(ui);
  if (!isFile(path.join(repo, "engine", "build", "kuma-preview"))) {
    out.push("the preview engine is not built — `cmake -S engine -B engine/build && cmake --build engine/build -j` (playback falls back to single frames without it)");
  }
  return out;
}
