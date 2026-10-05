// `m` — pins: your notes on frames, the one way the editor writes to the agent. A pin is a
// timeline marker with a comment (the format has no other kind), so an agent reading the
// project sees it, and `list_pins` lists them with ids it can answer in `apply_edits`.
//
// A pin is open, being read (the agent is reading pins now), or done in rN: a later
// revision said it answered it, or you ticked it. Done-ness is the daemon's session state,
// not the document's — the marker itself never changes when it is answered.

import { agentView } from "../agent";
import { Cmd, command } from "../commands";
import { button, el } from "../dom";
import { addPin, markerOn } from "../edits";
import { api } from "../api";
import { panels } from "../panelhost";
import { fmtTime, store, toast } from "../store";
import type { Marker, Pin } from "../types";

/** A pin's id, as the daemon names it: markers have none of their own. */
export const pinId = (sequence: string, t: number) => `${sequence}@${t.toFixed(3)}`;

/** The open sequence's pins, in time order, numbered as the daemon numbers them, with what
 *  the daemon knows of each. Read off the document, so they show with an older daemon too. */
export function sequencePins(): (Marker & { id: string; index: number; status: Pin | undefined })[] {
  const seq = store.state.sequence;
  const known = new Map(store.state.pins.map((p) => [p.id, p]));
  return store.markers
    .filter((m) => m.label.trim())
    .sort((a, b) => a.t - b.t)
    .map((m, i) => ({ ...m, id: pinId(seq, m.t), index: i + 1, status: known.get(pinId(seq, m.t)) }));
}

export function pinsPanel(): { body: HTMLElement; note: HTMLElement } {
  const body = el("div", "pins-panel");
  const note = el("span", "");
  const list = el("div", "pin-list");
  // The draft: "+ pin at playhead", which becomes a field when pressed (or on P).
  const draft = el("div", "pin-draft");
  // A textarea, because a note on a frame is a sentence or two: Enter sends it, Shift+Enter
  // starts another line.
  const input = el("textarea");
  input.rows = 1;
  let at = 0;
  const idle = () => {
    draft.classList.remove("editing");
    draft.replaceChildren(el("span", "pin-plus", "+"), el("span", "", `pin at ${fmtTime(store.state.playhead, store.fps)}`), el("kbd", "", "P"));
  };
  const begin = () => {
    at = store.snap(store.state.playhead);
    draft.classList.add("editing");
    input.value = markerOn(at) ?? "";
    input.placeholder = `what should change at ${fmtTime(at, store.fps)}?`;
    draft.replaceChildren(el("span", "pin-plus", "+"), input);
    input.focus();
  };
  draft.onclick = () => { if (!draft.classList.contains("editing")) begin(); };
  input.onkeydown = async (e) => {
    if (e.key === "Escape") { e.preventDefault(); idle(); return; }
    if (e.key !== "Enter" || e.shiftKey) return;
    e.preventDefault();
    if (await addPin(at, input.value)) idle();
  };
  input.onblur = () => { if (!input.value.trim()) setTimeout(idle, 0); };
  body.append(list, draft);

  command(Cmd.addPin, "Pin at playhead", () => {
    panels.show("pins");
    begin();
  });

  const draw = () => {
    const pins = sequencePins();
    note.textContent = pins.length ? String(pins.length) : "";
    const reading = /^reading pin (\d+)/.exec(agentView(store.state).doing)?.[1];
    if (!pins.length) list.replaceChildren(el("p", "muted tiny", "No pins on this sequence. A pin is a note on a frame for the agent: press P to leave one at the playhead."));
    else list.replaceChildren(...pins.map((p) => pinCard(p, String(p.index) === reading)));
    if (!draft.classList.contains("editing")) idle();
  };
  store.select((s) => [s.project, s.sequence, s.pins, s.activity], draw);
  store.select((s) => [s.playhead], () => { if (!draft.classList.contains("editing")) idle(); });
  return { body, note };
}

function pinCard(p: ReturnType<typeof sequencePins>[number], reading: boolean): HTMLElement {
  const done = p.status?.done ?? null;
  const card = el("div", `pin-card${done !== null ? " done" : reading ? " reading" : ""}`);
  card.appendChild(el("span", "pin-n", String(p.index)));
  const main = el("div", "pin-main");
  main.appendChild(el("div", "pin-text", p.label));
  const state = done !== null ? `done in r${done}${p.status?.done_by === "user" ? " · ticked" : ""}` : reading ? "kuma is reading it" : "open";
  main.appendChild(el("div", "pin-meta", `${+p.t.toFixed(2)}s · ${state}`));
  card.appendChild(main);
  // Ticking by hand is for an answer the agent did not say it gave.
  const tick = button(done !== null ? "↺" : "✓", async () => {
    try { await api.pinDone(p.id, done === null); store.loadPins(); } catch (e) { toast(`Cannot tick pins with this daemon: ${(e as Error).message}`, "err"); }
  }, done !== null ? "Open it again" : "Mark it done yourself", "pin-tick");
  tick.onpointerdown = (e) => e.stopPropagation();
  card.appendChild(tick);
  card.onclick = (e) => { if (!(e.target as HTMLElement).closest("button")) store.seek(p.t); };
  card.title = `${p.label}\n\n${p.t.toFixed(2)}s — click to go there`;
  return card;
}
