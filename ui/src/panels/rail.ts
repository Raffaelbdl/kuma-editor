// Power's rail: a letter per side panel, lit while that panel is on screen. The letters
// are the keys — the same ones that summon the panels in den — and each is a command: a
// docked panel comes to the front of its group, a closed one goes back where it was.

import { Cmd, run } from "../commands";
import { el } from "../dom";
import { icon } from "../icons";
import { keysFor } from "../shortcuts";
import { panels, type PanelSpec } from "../panelhost";
import { store } from "../store";

/** A letter per panel: pressed, it is the panel's letter (`panels.toggle`); lit while the
 *  panel is on screen. The rail's keys, and den's; `dress` adds what each says about itself. */
export function panelKeys(specs: PanelSpec[], cls: string, dress: (b: HTMLButtonElement, spec: PanelSpec) => void): HTMLButtonElement[] {
  const keys = specs.map((spec) => {
    const b = el("button", cls, spec.letter);
    b.dataset.panel = spec.id;
    b.onclick = () => panels.toggle(spec.id);
    dress(b, spec);
    return b;
  });
  const mark = () => { for (const b of keys) b.classList.toggle("on", panels.isVisible(b.dataset.panel!)); };
  document.addEventListener("kuma:panels", mark);
  mark();
  return keys;
}

export function mountRail(host: HTMLElement): void {
  // The side panels, top down, in the order they were registered.
  const keys = panelKeys(panels.all.filter((s) => s.dock === "side"), "rail-key", (b, spec) => {
    const key = keysFor(`panel.${spec.id}`);
    b.appendChild(el("span", "tip", `${spec.title}${key ? ` · ${key}` : ""}`));
  });
  host.append(...keys);
  // At the foot of the rail: whether the panels can be moved. Off, the dock holds still.
  const lock = el("button", "rail-key lock");
  lock.onclick = () => run(Cmd.movablePanels);
  host.appendChild(lock);
  let free: boolean | null = null;
  const markLock = () => {
    if (free === panels.canMove) return;
    free = panels.canMove;
    lock.replaceChildren(icon(free ? "unlock" : "lock", 14), el("span", "tip", free ? "panels move · click to lock them" : "panels locked · click to let them move"));
    lock.classList.toggle("free", free);
  };
  document.addEventListener("kuma:panels", markLock);
  markLock();
  // `m` has a dot while a marker on any sequence is waiting for the agent: the panel lists
  // them all, as `list_markers` does.
  const markers = keys.find((b) => b.dataset.panel === "markers");
  store.select((s) => [s.markerList], (s) => markers?.classList.toggle("flag", s.markerList.some((m) => m.waiting)));
}
