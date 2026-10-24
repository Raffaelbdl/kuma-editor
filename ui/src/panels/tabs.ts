// The editor tabs, in the top bar — Kdenlive's monitor and timeline tabs.
//
// Two kinds sit in one strip. A **sequence** tab is a timeline you have open: clicking it
// makes it the one being edited, which is a round trip because the daemon holds that
// choice and the engine holds its compiled timeline — so the tabs are a list of sequences
// one click away, not several running at once. An **entry** tab is a bin entry open on its
// own and in its own time: an overlay in the workbench, a picture, video or sound in the
// viewer.
//
// The strip only switches; what a tab shows is the panel's business. Which tabs exist is
// in the store because more than this bar reads it — the keyboard has to know which
// surface a key means, and the workbench and the viewer have to know when they are on screen.

import { Cmd, run } from "../commands";
import { button, el } from "../dom";
import { icon } from "../icons";
import { isMediaEntry, isOverlayEntry, SEQUENCE_TAB, store } from "../store";
import { itemKind, type ItemKind } from "../types";

interface Tab {
  label: string;
  /** The badge, which is also the kind of thing the tab opens. */
  kind: ItemKind;
  on: boolean;
  title: string;
  open(): void;
  /** Absent when this tab cannot be closed (the last sequence). */
  close?: { run(): void; title: string };
}

export function mountTabs(host: HTMLElement): void {
  const preview = document.getElementById("preview")!;
  const workbench = document.getElementById("workbench")!;
  const viewer = document.getElementById("viewer")!;
  const render = () => {
    const s = store.state;
    // Switching is all this bar does to the editors: every panel stays mounted, so the
    // engine keeps its socket and its overlays, and the workbench and the viewer keep
    // where you were in each entry. They drop the document or the file they are showing,
    // which costs a local reload and saves keeping a hidden one alive.
    preview.hidden = s.tab !== SEQUENCE_TAB;
    workbench.hidden = !isOverlayEntry(s.project, s.tab);
    viewer.hidden = !isMediaEntry(s.project, s.tab);
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
        close: s.sequences.length > 1 ? { run: () => void store.closeSequence(id), title: "Close this tab — or middle-click it (the sequence stays in the bin)" } : undefined,
      }));
    }
    for (const id of s.entries) {
      const item = s.project?.bin[id];
      if (!item) continue;
      host.appendChild(tab({
        label: name(id),
        kind: itemKind(item),
        on: s.tab === id,
        title: name(id),
        open: () => store.openEntry(id),
        close: { run: () => store.closeEntry(id), title: "Close this tab — or middle-click it (the entry stays in the bin)" },
      }));
    }
    host.appendChild(button(icon("plus"), () => run(Cmd.newOverlay), "New overlay — create one from a blueprint and open it here", "icon etab-add"));
  };
  store.select((s) => [s.project, s.sequence, s.sequences, s.tab, s.entries], render);
}

/** One tab. A `<div>` rather than a `<button>` because the close control is a button and
 *  a button cannot be nested in one. */
function tab(t: Tab): HTMLDivElement {
  const e = el("div", `etab${t.on ? " on" : ""}`);
  e.tabIndex = 0;
  e.title = t.title;
  const square = el("span", `kind-sq ${t.kind}`);
  square.title = t.kind;
  e.append(square, el("span", "etab-name", t.label));
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
    // A middle click closes, as in every browser and editor with tabs. The press is taken
    // so it does not start the browser's autoscroll first.
    e.onmousedown = (ev) => { if (ev.button === 1) ev.preventDefault(); };
    e.onauxclick = (ev) => { if (ev.button === 1) { ev.preventDefault(); close.run(); } };
  }
  return e;
}
