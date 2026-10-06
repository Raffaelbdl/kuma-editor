// Copy the launcher's static files next to its compiled JS, so `dist/launcher` is the
// one directory the `kuma-app://` handler serves.
//
// The launcher wears the editor's clothes, taken from the editor rather than kept in step
// by hand: its palette (`themes.css`), its three faces (`fonts.css` and the files it
// names), its top bar and status line (`chrome.css`) and the bear, all from `ui/src`.
import { cpSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const app = dirname(dirname(fileURLToPath(import.meta.url)));
const ui = join(dirname(app), "ui", "src");
const src = join(app, "launcher");
const dst = join(app, "dist", "launcher");
mkdirSync(dst, { recursive: true });
for (const name of readdirSync(src)) {
  if (name.endsWith(".ts")) continue;
  cpSync(join(src, name), join(dst, name), { recursive: true });
}
for (const css of ["themes.css", "fonts.css", "chrome.css"]) cpSync(join(ui, css), join(dst, css));
cpSync(join(ui, "fonts"), join(dst, "fonts"), { recursive: true });
for (const bear of ["bear.png", "bear-sleep.png", "bear-look.png"]) cpSync(join(ui, "assets", bear), join(dst, bear));
console.log(`copied launcher assets → ${dst}`);
