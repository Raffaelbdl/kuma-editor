// Power's rail: a letter per side panel. The letters are the keys — the same ones that
// summon the panels in den — and each is a command, so the rail is one more way to run
// them rather than a second set of switches.

import { el } from "../dom";
import { keysFor } from "../shortcuts";
import { panels } from "../panelhost";
import { store } from "../store";

/** The side panels, top down. `m` lights up while a pin is open. */
const RAIL = ["bin", "blueprints", "fonts", "pins", "history", "queue"];

export function mountRail(host: HTMLElement): void {
  const keys = RAIL.map((id) => {
    const spec = panels.spec(id)!;
    const b = el("button", "rail-key", spec.letter);
    const key = keysFor(`panel.${id}`);
    b.appendChild(el("span", "tip", `${spec.title}${key ? ` · ${key}` : ""}`));
    b.onclick = () => panels.toggle(id);
    b.dataset.panel = id;
    host.appendChild(b);
    return b;
  });
  const mark = () => {
    for (const b of keys) b.classList.toggle("on", panels.rail === b.dataset.panel && !panels.isOut(b.dataset.panel!));
    const open = store.state.pins.some((p) => p.done === null);
    keys[RAIL.indexOf("pins")].classList.toggle("flag", open);
  };
  document.addEventListener("kuma:panels", mark);
  store.select((s) => [s.pins], mark);
}
