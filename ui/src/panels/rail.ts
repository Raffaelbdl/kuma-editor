// Power's rail: a letter per side panel, lit while that panel is on screen. The letters
// are the keys — the same ones that summon the panels in den — and each is a command: a
// docked panel comes to the front of its group, a closed one goes back where it was.

import { Cmd, run } from "../commands";
import { el } from "../dom";
import { icon } from "../icons";
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
  // At the foot of the rail: whether the panels can be moved. Off, the dock holds still.
  const lock = el("button", "rail-key lock");
  lock.onclick = () => run(Cmd.movablePanels);
  host.appendChild(lock);
  const mark = () => {
    const free = panels.canMove;
    lock.replaceChildren(icon(free ? "unlock" : "lock", 14), el("span", "tip", free ? "panels move · click to lock them" : "panels locked · click to let them move"));
    lock.classList.toggle("free", free);
    for (const b of keys) b.classList.toggle("on", panels.isVisible(b.dataset.panel!));
    const open = store.state.pins.some((p) => p.done === null);
    keys[RAIL.indexOf("pins")].classList.toggle("flag", open);
  };
  document.addEventListener("kuma:panels", mark);
  store.select((s) => [s.pins], mark);
}
