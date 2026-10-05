// `f` — the fonts a `font` var can name: the project's own (which win) and the library's,
// each with the writing systems it sets. Adding one is a file, not an edit (the desktop
// shell's picker; an agent's `add_font`).

import { api } from "../api";
import { desktop } from "../desktop";
import { button, el } from "../dom";
import { store, toast } from "../store";

export function fontsPanel(): { body: HTMLElement; tools: HTMLElement } {
  const body = el("div", "fonts-panel");
  const tools = el("span", "lib-tools");
  if (desktop()) {
    tools.appendChild(button("+", async () => {
      const app = desktop();
      if (!app) return;
      for (const path of await app.pickFonts()) {
        try { const r = await api.addFont(path, false); toast(`Added ${r.family}`, "ok"); } catch (e) { toast((e as Error).message, "err"); }
      }
      await store.reloadFonts();
    }, "Add a font file to this project", "tool"));
  }
  store.select((s) => [s.fonts], (s) => {
    if (!s.fonts.length) { body.replaceChildren(el("p", "muted tiny", "No fonts: `kuma library` installs the starter families.")); return; }
    body.replaceChildren(...s.fonts.map((f) => {
      const row = el("div", "font-row");
      const name = el("div", "font-name", f.name);
      name.style.fontFamily = `"${f.name}", var(--mono)`;
      row.append(name, el("div", "font-meta", `${f.origin} · ${f.scripts.join(", ") || "latin"} · ${f.faces.length} face${f.faces.length === 1 ? "" : "s"}`));
      return row;
    }));
  });
  return { body, tools };
}
