// Creating a project. The shell does not write `.kuma.json` itself: it runs `kuma init`,
// so a project made from the launcher is byte-for-byte one made from the CLI — including
// the project-scoped `.mcp.json` that lets Claude Code join it.

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

/** Run `kuma init` and return the project file it created. */
export async function create(bin: string, repo: string, spec: NewProject): Promise<string> {
  const file = projectFileFor(spec.parentDir, spec.name);
  if (fs.existsSync(file)) throw new Error(`${file} already exists.`);
  const dir = path.dirname(file);
  // `kuma init` creates the directory, but refuses if the file is there; check the folder
  // too so "New project" never half-fills a directory someone is already using.
  if (fs.existsSync(dir) && fs.readdirSync(dir).length) {
    throw new Error(`${dir} already exists and is not empty.`);
  }
  const args = ["init", "--project", file, "--width", String(spec.width), "--height", String(spec.height), "--fps", spec.fps];
  try {
    await run(bin, args, { env: { ...process.env, KUMA_REPO: repo }, timeout: 30_000 });
  } catch (e) {
    const err = e as { stderr?: string; message?: string };
    throw new Error(`kuma init failed: ${(err.stderr || err.message || "").trim()}`);
  }
  if (!fs.existsSync(file)) throw new Error(`kuma init reported success but ${file} is not there.`);
  return file;
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
  // Enough of a check to catch "you picked the wrong .json", not a substitute for the
  // daemon's own validation, which runs when it loads the file a moment later.
  const p = parsed as { bin?: unknown; master?: unknown };
  if (typeof p !== "object" || p === null || typeof p.master !== "string" || typeof p.bin !== "object") {
    throw new Error(`${path.basename(file)} does not look like a kuma project (no \`master\` and \`bin\`).`);
  }
}
