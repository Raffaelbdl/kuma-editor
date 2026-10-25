// ⌘K: every command by name. The list is the registry (`commands.ts`) — what the keys, the
// rail, the toolbar and the shell's menus run — plus the sequences to open and the markers to
// go to, so there is nothing here that is not reachable some other way, and nothing that
// is reachable some other way and missing here.

import { Cmd, command, list, run } from "./commands";
import { markersOn } from "./agent";
import { el } from "./dom";
import { keysFor } from "./shortcuts";
import { sequenceLabel, store } from "./store";
import { sequenceIds } from "./types";

interface Entry { label: string; keys?: string; run(): void }

let open: (() => void) | null = null;

function entries(): Entry[] {
  const out: Entry[] = list()
    // Ones that need an argument from somewhere else (a step, a scale, a kind) are not
    // commands a name can run on its own.
    .filter((c) => ![Cmd.seekEdge, Cmd.previewScale, Cmd.overlayStep, Cmd.viewerStep, Cmd.addTrack, Cmd.palette].includes(c.id as never))
    .map((c) => ({ label: c.label, keys: keysFor(c.id), run: () => run(c.id) }));
  for (const kind of ["video", "overlay", "audio"]) out.push({ label: `Add ${kind} track`, run: () => run(Cmd.addTrack, kind) });
  const p = store.state.project;
  if (p) {
    for (const id of sequenceIds(p)) out.push({ label: `Open sequence: ${sequenceLabel(id)}`, run: () => void store.openSequence(id) });
    for (const m of markersOn(store.state.markerList, store.state.sequence)) out.push({ label: `Go to marker: ${m.label.split("\n")[0] || `#${m.index}`}`, run: () => store.seek(m.t) });
  }
  return out;
}

/** Letters in order, not necessarily together: "spl" finds "Split at Playhead". Earlier and
 *  tighter matches first. */
function score(label: string, q: string): number {
  const l = label.toLowerCase();
  if (!q) return 0;
  const at = l.indexOf(q);
  if (at >= 0) return at === 0 || l[at - 1] === " " ? 1000 - at : 500 - at;
  let i = 0, gaps = 0, last = -1;
  for (const ch of q) {
    const j = l.indexOf(ch, i);
    if (j < 0) return -1;
    if (last >= 0) gaps += j - last - 1;
    last = j;
    i = j + 1;
  }
  return 100 - gaps;
}

export function openPalette(): void {
  if (open) { open(); return; }
  const scrim = el("div", "palette-scrim");
  const box = el("div", "palette");
  const input = el("input");
  input.type = "text";
  input.placeholder = "split, add track, render…";
  const ul = el("div", "palette-list");
  box.append(input, ul);
  scrim.appendChild(box);
  document.body.appendChild(scrim);
  const all = entries();
  let shown: Entry[] = [];
  let cur = 0;
  const draw = () => {
    const q = input.value.trim().toLowerCase();
    shown = all.map((e) => ({ e, s: score(e.label, q) })).filter((x) => x.s >= 0).sort((a, b) => b.s - a.s).map((x) => x.e).slice(0, 40);
    cur = Math.min(cur, Math.max(0, shown.length - 1));
    ul.replaceChildren(...shown.map((e, i) => {
      const row = el("div", `palette-row${i === cur ? " on" : ""}`);
      row.append(el("span", "", e.label));
      if (e.keys) row.append(el("kbd", "", e.keys));
      row.onpointerdown = (ev) => { ev.preventDefault(); choose(i); };
      return row;
    }));
    if (!shown.length) ul.appendChild(el("div", "palette-empty", "Nothing by that name"));
    ul.children[cur]?.scrollIntoView({ block: "nearest" });
  };
  const close = () => { scrim.remove(); open = null; };
  const choose = (i: number) => { const e = shown[i]; close(); e?.run(); };
  input.oninput = () => { cur = 0; draw(); };
  input.onkeydown = (e) => {
    if (e.key === "Escape") { e.preventDefault(); close(); }
    else if (e.key === "ArrowDown") { e.preventDefault(); cur = Math.min(shown.length - 1, cur + 1); draw(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); cur = Math.max(0, cur - 1); draw(); }
    else if (e.key === "Enter") { e.preventDefault(); choose(cur); }
  };
  scrim.onpointerdown = (e) => { if (e.target === scrim) close(); };
  open = close;
  draw();
  input.focus();
}

export function installPalette(): void {
  command(Cmd.palette, "Command palette", () => openPalette());
}
