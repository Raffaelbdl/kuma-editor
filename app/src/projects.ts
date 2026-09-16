// Creating and vetting a project. The shell does not write `.kuma.json` itself: it runs
// `kuma init`, so a project made from the launcher is byte-for-byte one made from the CLI
// — including the project-scoped `.mcp.json` that lets Claude Code join it.

import { execFile } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { promisify } from "node:util";

const run = promisify(execFile);

export interface NewProject {
  /** Directory the project folder is created in. */
  parentDir: string;
  /** Folder and file name; `<parentDir>/<name>/<name>.kuma.json`. */
  name: string;
  width: number;
  height: number;
  /** `30`, `29.97` or `30000/1001` — passed through to `kuma init` as typed. */
  fps: string;
}

/** Slug for a folder and a `<name>.kuma.json` that will not need quoting. */
export function slug(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64);
}

export function projectFileFor(parentDir: string, name: string): string {
  const s = slug(name) || "untitled";
  return path.join(parentDir, s, `${s}.kuma.json`);
}

/**
 * Run `kuma init` and return the project file it created.
 *
 * What `kuma init` writes is checked before returning, because a `kuma` binary older than
 * this checkout writes a project in a format nothing here can open. Leaving that on disk
 * is worse than not creating it: the folder then blocks a retry under the same name while
 * being useless. So a directory this call created is removed again when the result does
 * not open.
 */
export async function create(bin: string, repo: string, spec: NewProject): Promise<string> {
  const file = projectFileFor(spec.parentDir, spec.name);
  const dir = path.dirname(file);
  if (fs.existsSync(file)) {
    throw new Error(`${file} already exists.\n\nPick a different name, or move that folder aside.`);
  }
  // `kuma init` creates the directory, but refuses if the file is there; check the folder
  // too so "New project" never half-fills a directory someone is already using.
  const dirExisted = fs.existsSync(dir);
  if (dirExisted && fs.readdirSync(dir).length) {
    throw new Error(`${dir} already exists and is not empty.\n\nPick a different name, or move that folder aside.`);
  }

  const args = ["init", "--project", file, "--width", String(spec.width), "--height", String(spec.height), "--fps", spec.fps];
  try {
    await run(bin, args, { env: { ...process.env, KUMA_REPO: repo }, timeout: 30_000 });
  } catch (e) {
    const err = e as { stderr?: string; message?: string };
    throw new Error(`kuma init failed: ${(err.stderr || err.message || "").trim()}`);
  }
  if (!fs.existsSync(file)) throw new Error(`kuma init reported success but ${file} is not there.`);

  try {
    checkOpenable(file);
  } catch (e) {
    undo(dir, dirExisted, file);
    throw new Error(`\`${bin}\` created a project this version cannot open.\n\n${(e as Error).message}`);
  }
  return file;
}

/** Roll back what `create` made, and nothing it did not. */
function undo(dir: string, dirExisted: boolean, file: string) {
  try {
    if (dirExisted) fs.rmSync(file, { force: true });
    else fs.rmSync(dir, { recursive: true, force: true });
  } catch {
    /* Leaving it behind is not worth failing over; the real error is the caller's. */
  }
}

/** What `open` accepts. A project is one JSON file; the folder around it is convention. */
export const PROJECT_FILTERS = [
  { name: "kuma project", extensions: ["json"] },
  { name: "All files", extensions: ["*"] },
];

/** Reject anything that is not a readable project file, with a reason worth showing. */
export function checkOpenable(file: string): void {
  if (!fs.existsSync(file)) throw new Error(`${file} does not exist.`);
  let text: string;
  try {
    text = fs.readFileSync(file, "utf8");
  } catch (e) {
    throw new Error(`Cannot read ${file}: ${(e as Error).message}`);
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    throw new Error(`${path.basename(file)} is not valid JSON: ${(e as Error).message}`);
  }
  const name = path.basename(file);
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error(`${name} is not a kuma project (its top level is not an object).`);
  }
  const p = parsed as Record<string, unknown>;

  // The format before sequences became bin entries kept `tracks` and `assets` on the
  // project. Nothing here reads it, and the daemon refuses it outright — but the reason
  // is almost always a `kuma` binary older than this checkout, so say that rather than
  // "not a kuma project", which sends people looking at the file.
  if (Array.isArray(p.tracks) && p.bin === undefined) {
    throw new Error(
      `${name} is in the old project format (\`tracks\` on the project instead of a \`bin\`).\n\n` +
        "It was written by a `kuma` binary older than this checkout. Rebuild it with " +
        "`cargo build --release` and try again. There is no upgrade path for the file itself.",
    );
  }
  // Enough of a check to catch "you picked the wrong .json", not a substitute for the
  // daemon's own validation, which runs when it loads the file a moment later.
  const missing = [
    typeof p.master === "string" ? "" : "master",
    typeof p.bin === "object" && p.bin !== null ? "" : "bin",
  ].filter(Boolean);
  if (missing.length) {
    const keys = Object.keys(p).slice(0, 12).join(", ") || "(none)";
    throw new Error(`${name} does not look like a kuma project: no \`${missing.join("` and `")}\`.\n\nIt has: ${keys}`);
  }
}
