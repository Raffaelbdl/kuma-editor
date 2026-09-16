// `npm run app` should work in a fresh checkout. The shell's own dependencies (electron,
// typescript) are not installed by the repository root's `npm install`, so check for them
// here and install them once rather than failing with `tsc: not found`.
import { existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const app = dirname(dirname(fileURLToPath(import.meta.url)));
const needed = ["typescript", "electron"].filter((p) => !existsSync(join(app, "node_modules", p)));
if (!needed.length) process.exit(0);

console.log(`kuma app: installing ${needed.join(", ")} (first run)…`);
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const r = spawnSync(npm, ["install", "--no-audit", "--no-fund"], { cwd: app, stdio: "inherit" });
if (r.status !== 0) {
  console.error("\nkuma app: `npm install` failed in app/. Run it there by hand to see why.");
  process.exit(r.status ?? 1);
}
