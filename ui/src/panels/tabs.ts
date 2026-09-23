// The editor tabs, over the centre of the window — Kdenlive's monitor and timeline tabs.
//
// Two kinds sit in one strip. A **sequence** tab is a timeline you have open: clicking it
// makes it the one being edited, which is a round trip because the daemon holds that
// choice and the engine holds its compiled timeline — so the tabs are a list of sequences
// one click away, not several running at once. An **overlay** tab is a bin entry open in
// the workbench, on its own and in its own time.
//
// The strip only switches; what a tab shows is the panel's business. Which tabs exist is
// in the store because more than this bar reads it — the keyboard has to know which
// surface a key means, and the workbench has to know when it is on screen.

import { Cmd, run } from "../commands";
import { button, el } from "../dom";
import { SEQUENCE_TAB, store } from "../store";

interface Tab {
  label: string;
  /** The badge, which is also the kind of thing the tab opens. */
  kind: "sequence" | "overlay";
  on: boolean;
  title: string;
  open(): void;
  /** Absent when this tab cannot be closed (the last sequence). */
  close?: { run(): void; title: string };
}

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

    const name = (id: string) => s.project?.bin[id]?.name || id;
    for (const id of s.sequences) {
      const master = s.project?.master === id;
      host.appendChild(tab({
        label: name(id),
        kind: "sequence",
        // The sequence tab that is on is the one the daemon has open — and only while the
        // monitor is what the centre is showing.
        on: s.tab === SEQUENCE_TAB && s.sequence === id,
        title: master ? `${name(id)} — the master, which is what renders` : name(id),
        open: () => void store.openSequence(id),
        // There is always one sequence open, so the last tab has nothing to fall back to.
        close: s.sequences.length > 1 ? { run: () => void store.closeSequence(id), title: "Close this tab (the sequence stays in the bin)" } : undefined,
      }));
    }
    for (const id of s.overlays) {
      host.appendChild(tab({
        label: name(id),
        kind: "overlay",
        on: s.tab === id,
        title: name(id),
        open: () => store.openOverlay(id),
        close: { run: () => store.closeOverlay(id), title: "Close this tab (the entry stays in the bin)" },
      }));
    }
    host.appendChild(button("+ overlay", () => run(Cmd.newOverlay), "Create an overlay from a blueprint and open it here", "etab-add"));
  };
  store.select((s) => [s.project, s.sequence, s.sequences, s.tab, s.overlays], render);
}

/** One tab. A `<div>` rather than a `<button>` because the close control is a button and
 *  a button cannot be nested in one. */
function tab(t: Tab): HTMLDivElement {
  const e = el("div", `etab${t.on ? " on" : ""}`);
  e.tabIndex = 0;
  e.title = t.title;
  e.append(el("span", `badge-kind ${t.kind}`, t.kind), el("span", "etab-name", t.label));
  e.onclick = () => t.open();
  e.onkeydown = (ev) => { if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); t.open(); } };
  if (t.close) {
    const close = t.close;
    const x = el("button", "x", "×");
    x.title = close.title;
    // The × is inside the tab, so its click must not also reach the tab — closing a tab
    // and immediately reopening it is not a close. Stopping `pointerdown` is not enough:
    // the click still bubbles after it.
    x.onclick = (ev) => { ev.stopPropagation(); close.run(); };
    e.appendChild(x);
  }
  return e;
}
