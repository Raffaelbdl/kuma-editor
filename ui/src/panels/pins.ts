// `m` — pins: your notes on frames, the one way the editor writes to the agent. A pin is a
// timeline marker with a comment (the format has no other kind), so an agent reading the
// project sees it, and `list_pins` lists them with ids it can answer in `apply_edits`.
//
// A pin is open, being read (the agent is reading pins now), or done in rN: a later
// revision said it answered it, or you ticked it. Done-ness is the daemon's session state,
// not the document's — the marker itself never changes when it is answered.

import { watchAgent } from "../agent";
import { Cmd, command } from "../commands";
import { button, el } from "../dom";
import { addPin, markerOn } from "../edits";
import { api } from "../api";
import { panels } from "../panelhost";
import { keysFor } from "../shortcuts";
import { fmtTime, store, toast } from "../store";
import type { Pin } from "../types";

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
  // Idle, the draft is these three, and only the time in it follows the playhead.
  const when = el("span");
  const offer = [el("span", "pin-plus", "+"), when, el("kbd", "", keysFor(Cmd.addPin) ?? "P")];
  const editing = () => draft.classList.contains("editing");
  const idle = () => {
    draft.classList.remove("editing");
    draft.replaceChildren(...offer);
  };
  const begin = () => {
    at = store.snap(store.state.playhead);
    draft.classList.add("editing");
    input.value = markerOn(at) ?? "";
    input.placeholder = `what should change at ${fmtTime(at, store.fps)}?`;
    draft.replaceChildren(el("span", "pin-plus", "+"), input);
    input.focus();
  };
  draft.onclick = () => { if (!editing()) begin(); };
  input.onkeydown = async (e) => {
    if (e.key === "Escape") { e.preventDefault(); idle(); return; }
    if (e.key !== "Enter" || e.shiftKey) return;
    e.preventDefault();
    if (await addPin(at, input.value)) idle();
  };
  input.onblur = () => { if (!input.value.trim()) setTimeout(idle, 0); };
  body.append(list, draft);
  idle();

  command(Cmd.addPin, "Pin at playhead", () => {
    panels.show("pins");
    begin();
  });

  // The daemon's pins: it numbers them and knows which are answered (`pins.rs`).
  let reading: string | undefined;
  const draw = () => {
    const pins = store.state.pins.filter((p) => p.sequence === store.state.sequence);
    note.textContent = pins.length ? String(pins.length) : "";
    if (!pins.length) list.replaceChildren(el("p", "muted tiny", "No pins on this sequence. A pin is a note on a frame for the agent: press P to leave one at the playhead."));
    else list.replaceChildren(...pins.map((p) => pinCard(p, p.id === reading)));
  };
  store.select((s) => [s.pins, s.sequence], draw);
  watchAgent((v) => { if (v.pin?.id !== reading) { reading = v.pin?.id; draw(); } });
  store.select((s) => [s.playhead], (s) => { when.textContent = `pin at ${fmtTime(s.playhead, store.fps)}`; });
  return { body, note };
}

function pinCard(p: Pin, reading: boolean): HTMLElement {
  const done = p.done;
  const card = el("div", `pin-card${done !== null ? " done" : reading ? " reading" : ""}`);
  card.appendChild(el("span", "pin-n", String(p.index)));
  const main = el("div", "pin-main");
  main.appendChild(el("div", "pin-text", p.label));
  const state = done !== null ? `done in r${done}${p.done_by === "user" ? " · ticked" : ""}` : reading ? "kuma is reading it" : "open";
  main.appendChild(el("div", "pin-meta", `${+p.t.toFixed(2)}s · ${state}`));
  card.appendChild(main);
  // Ticking by hand is for an answer the agent did not say it gave. The daemon tells every
  // client, this one included, with the new list.
  const tick = button(done !== null ? "↺" : "✓", async () => {
    try { await api.pinDone(p.id, done === null); } catch (e) { toast(`Cannot tick pins with this daemon: ${(e as Error).message}`, "err"); }
  }, done !== null ? "Open it again" : "Mark it done yourself", "pin-tick");
  tick.onpointerdown = (e) => e.stopPropagation();
  card.appendChild(tick);
  card.onclick = (e) => { if (!(e.target as HTMLElement).closest("button")) store.seek(p.t); };
  card.title = `${p.label}\n\n${p.t.toFixed(2)}s — click to go there`;
  return card;
}
