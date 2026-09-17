// The editor tabs, over the centre of the window — Kdenlive's monitor tabs.
//
// One tab is the **sequence** being edited: the engine's picture with the open
// sequence's overlays live on top, driven by the timeline below. Every other tab is an
// **overlay entry** open in the workbench, on its own and in its own time.
//
// The strip only switches; what a tab shows is the panel's business. Which tabs exist is
// in the store because more than this bar reads it — the keyboard has to know which
// surface a key means, and the workbench has to know when it is on screen.

import { Cmd, run } from "../commands";
import { button, el } from "../dom";
import { SEQUENCE_TAB, store } from "../store";

export function mountTabs(host: HTMLElement): void {
  const preview = document.getElementById("preview")!;
  const workbench = document.getElementById("workbench")!;
  const render = () => {
    const s = store.state;
    // Switching is all this bar does to the editors: both panels stay mounted, so the
    // engine keeps its socket and its overlays, and the workbench keeps where you were in
    // each entry. It drops the document it is showing, which costs a local reload and
    // saves keeping a hidden one alive.
    preview.hidden = s.tab !== SEQUENCE_TAB;
    workbench.hidden = s.tab === SEQUENCE_TAB;
    host.replaceChildren();
    const seqName = (s.project && s.sequence && (s.project.bin[s.sequence]?.name || s.sequence)) || "Sequence";
    host.appendChild(tab(SEQUENCE_TAB, seqName, "sequence", s.tab === SEQUENCE_TAB));
    for (const id of s.overlays) {
      host.appendChild(tab(id, s.project?.bin[id]?.name || id, "overlay", s.tab === id, true));
    }
    host.appendChild(button("+ overlay", () => run(Cmd.newOverlay), "Create an overlay from a template and open it here", "etab-add"));
  };
  store.select((s) => [s.project, s.sequence, s.tab, s.overlays], render);
}

/** One tab. A `<div>` rather than a `<button>` because the close control is a button and
 *  a button cannot be nested in one. */
function tab(id: string, label: string, kind: string, on: boolean, closable = false): HTMLDivElement {
  const t = el("div", `etab${on ? " on" : ""}`);
  t.tabIndex = 0;
  t.title = label;
  t.append(el("span", `badge-kind ${kind}`, kind), el("span", "etab-name", label));
  t.onclick = () => store.showTab(id);
  t.onkeydown = (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); store.showTab(id); } };
  if (closable) {
    const x = button("×", () => store.closeOverlay(id), "Close this tab (the entry stays in the bin)", "x");
    x.onpointerdown = (e) => e.stopPropagation();
    t.appendChild(x);
  }
  return t;
}
