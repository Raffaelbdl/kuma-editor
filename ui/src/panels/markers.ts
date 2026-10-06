// `m` — markers: moments on the open sequence, each with a comment or none. A person's
// marker with a comment is how the editor writes to the agent: `list_markers` lists it as
// waiting, with an id the agent answers in `apply_edits`. The agent leaves markers too —
// its notes, a TODO, the two edges of a zone — and the daemon stamps who left each
// (`origin`), so its own are never read back to it as feedback.
//
// A person's comment is waiting, being read (the agent is listing markers now), or answered
// in rN: a later revision said it answered it, or you ticked it. That is the daemon's session
// state, not the document's — the marker itself never changes when it is answered.

import { markersOn, who, watchAgent } from "../agent";
import { Cmd, command } from "../commands";
import { button, el, setText } from "../dom";
import { markerOn, placeMarker } from "../edits";
import { api } from "../api";
import { panels } from "../panelhost";
import { keysFor } from "../shortcuts";
import { fmtTime, store, toast } from "../store";
import type { MarkerInfo } from "../types";

export function markersPanel(): { body: HTMLElement; note: HTMLElement } {
  const body = el("div", "markers-panel");
  const note = el("span", "");
  const list = el("div", "mk-list");
  // The draft: "+ marker at playhead", which becomes a field when pressed (or on P).
  const draft = el("div", "mk-draft");
  // A textarea, because a note on a frame is a sentence or two: Enter sends it, Shift+Enter
  // starts another line.
  const input = el("textarea");
  input.rows = 1;
  let at = 0;
  // Idle, the draft is these three, and only the time in it follows the playhead.
  const when = el("span");
  const offer = [el("span", "mk-plus", "+"), when, el("kbd", "", keysFor(Cmd.addMarker) ?? "P")];
  const editing = () => draft.classList.contains("editing");
  const idle = () => {
    draft.classList.remove("editing");
    draft.replaceChildren(...offer);
  };
  /** Write the marker at `t` (the playhead unless the ruler named a moment). */
  const begin = (t = store.state.playhead) => {
    at = store.snap(t);
    draft.classList.add("editing");
    input.value = markerOn(at) ?? "";
    input.placeholder = `a note at ${fmtTime(at, store.fps)} — for kuma, or for you`;
    draft.replaceChildren(el("span", "mk-plus", "+"), input);
    input.focus();
  };
  draft.onclick = () => { if (!editing()) begin(); };
  input.onkeydown = async (e) => {
    if (e.key === "Escape") { e.preventDefault(); idle(); return; }
    if (e.key !== "Enter" || e.shiftKey) return;
    e.preventDefault();
    if (await placeMarker(at, input.value)) idle();
  };
  input.onblur = () => { if (!input.value.trim()) setTimeout(idle, 0); };
  body.append(list, draft);
  idle();

  // P at the playhead; the ruler's right-click names its own moment.
  command(Cmd.addMarker, "Marker at playhead", (t) => {
    panels.show("markers");
    begin(typeof t === "number" ? t : undefined);
  });

  // The daemon's list: it numbers them, says who left each and which are waiting (`markers.rs`).
  let reading: string | undefined;
  const draw = () => {
    const markers = markersOn(store.state.markerList, store.state.sequence);
    const waiting = markers.filter((m) => m.waiting).length;
    note.textContent = waiting ? `${waiting} waiting` : markers.length ? String(markers.length) : "";
    if (!markers.length) list.replaceChildren(el("p", "muted tiny", "No markers on this sequence. Press P to leave one at the playhead: with a comment, it is a note for the agent."));
    else list.replaceChildren(...markers.map((m) => markerCard(m, m.id === reading)));
  };
  store.select((s) => [s.markerList, s.sequence], draw);
  watchAgent((v) => { if (v.marker?.id !== reading) { reading = v.marker?.id; draw(); } });
  // The draft's time follows the playhead only while anyone can read it.
  store.select((s) => [s.playhead], panels.lazily("markers", () => setText(when, `marker at ${fmtTime(store.state.playhead, store.fps)}`)).run);
  return { body, note };
}

function markerCard(m: MarkerInfo, reading: boolean): HTMLElement {
  const mine = m.author !== "agent";
  const done = m.done;
  const card = el("div", `mk-card${mine ? "" : " agent"}${done !== null ? " done" : reading ? " reading" : ""}`);
  card.appendChild(el("span", "mk-n", String(m.index)));
  const main = el("div", "mk-main");
  main.appendChild(el("div", m.label ? "mk-text" : "mk-text muted", m.label || "no comment"));
  // What it is to the agent: its own note, a person's comment waiting or answered, or just a
  // moment marked (no comment, nothing to answer).
  const state = !mine ? `${who("agent")}'s note`
    : done !== null ? `answered in r${done}${m.done_by === "user" ? " · ticked" : ""}`
    : reading ? `${who("agent")} is reading it`
    : m.waiting ? "waiting" : "a moment marked";
  main.appendChild(el("div", "mk-meta", `${fmtTime(m.t, store.fps)} · ${state}`));
  card.appendChild(main);
  // Ticking by hand is for an answer the agent did not say it gave — on a comment left for
  // it. The daemon tells every client, this one included, with the new list.
  if (mine && m.label) {
    const tick = button(done !== null ? "↺" : "✓", async () => {
      try { await api.markerDone(m.id, done === null); } catch (e) { toast(`Cannot tick markers with this daemon: ${(e as Error).message}`, "err"); }
    }, done !== null ? "Waiting again" : "Mark it answered yourself", "mk-tick");
    tick.onpointerdown = (e) => e.stopPropagation();
    card.appendChild(tick);
  }
  card.onclick = (e) => { if (!(e.target as HTMLElement).closest("button")) store.seek(m.t); };
  card.title = `${m.label || "no comment"}\n\n${fmtTime(m.t, store.fps)} — click to go there`;
  return card;
}
