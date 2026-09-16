// Copy the launcher's static files next to its compiled JS, so `dist/launcher` is the
// one directory the `kuma-app://` handler serves.
import { cpSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const app = dirname(dirname(fileURLToPath(import.meta.url)));
const src = join(app, "launcher");
const dst = join(app, "dist", "launcher");
mkdirSync(dst, { recursive: true });
for (const name of readdirSync(src)) {
  if (name.endsWith(".ts")) continue;
  cpSync(join(src, name), join(dst, name), { recursive: true });
}
console.log(`copied launcher assets → ${dst}`);
