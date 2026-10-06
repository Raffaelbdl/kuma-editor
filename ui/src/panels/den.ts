// Den: the frame, full-bleed, and what the agent did and where it looked.
//
// Over the picture: the bear and the mode, the sequence and what the agent is doing (top
// left); a way back to power (top right); orange corners round the region the agent last
// sampled; and the summary of its last edit as a caption. Under it, a rail: play, the time,
// the sequence with its revisions placed where they edited and its markers, and the keys that
// summon the panels. Everything here reads the store; none of it is the document.

import { agentView, bear, markersOn, setBear, watchAgent } from "../agent";
import { Cmd, run } from "../commands";
import { button, dragPointer, el, setText } from "../dom";
import { panels } from "../panelhost";
import { keysFor } from "../shortcuts";
import { panelKeys } from "./rail";
import { playButton } from "./transport";
import { binName, fmtTime, store } from "../store";
import type { Activity, HistoryEntry } from "../types";

export function mountDen(host: HTMLElement): void {
  host.append(el("div", "den-scrim"));

  // ---- HUD
  const pet = bear("idle", 3);
  const seqName = el("span", "den-seq");
  const status = el("span", "den-status");
  const cursor = el("span", "den-cursor", "_");
  const hud = el("div", "den-hud");
  hud.append(pet, el("span", "den-chip", "den"), seqName, status, cursor);
  const power = button("power ↗", () => panels.setMode("power"), "Back to every panel, at the playhead (the selection stays)", "den-power");
  host.append(hud, power);
  const drawStatus = (v = agentView(store.state)) => {
    const p = store.state.project;
    status.textContent = `${p ? `r${p.revision}` : ""}${v.client ? ` · kuma ${v.doing}` : ""}`;
  };
  watchAgent((v) => {
    setBear(pet, v.bear);
    drawStatus(v);
    // The cursor blinks while the agent is at work; asleep, it holds still and the page
    // has nothing to draw.
    cursor.classList.toggle("live", v.bear !== "idle");
  });
  store.select((s) => [s.project, s.sequence], (s) => { seqName.textContent = binName(s.sequence); drawStatus(); });

  // ---- viewfinder: the corners of what the agent last looked at
  const vf = el("div", "den-vf");
  for (const c of ["tl", "tr", "bl", "br"]) vf.appendChild(el("span", `vf-c ${c}`));
  host.appendChild(vf);
  const placeFinder = () => {
    const stage = document.querySelector<HTMLElement>("#preview .stage");
    const p = store.state.project;
    if (!stage || !p || store.state.mode !== "den" || !stage.offsetWidth) { vf.hidden = true; return; }
    const box = stage.getBoundingClientRect(), mine = host.getBoundingClientRect();
    const look = lastLook(store.state.activity, store.state.sequence, p.master);
    const r = look?.sample?.rect ?? { x: 0, y: 0, w: p.width, h: p.height };
    const sx = box.width / p.width, sy = box.height / p.height;
    // Inset a little when it is the whole frame, so the corners sit on the picture.
    const inset = look?.sample?.rect ? 0 : 14;
    Object.assign(vf.style, {
      left: `${box.left - mine.left + r.x * sx + inset}px`, top: `${box.top - mine.top + r.y * sy + inset}px`,
      width: `${r.w * sx - 2 * inset}px`, height: `${r.h * sy - 2 * inset}px`,
    });
    vf.hidden = false;
    vf.title = look ? `Where kuma last looked: sample_frames${look.sample?.rect ? ` of ${r.w}×${r.h} at ${r.x},${r.y}` : ""}` : "";
  };
  const observer = new ResizeObserver(() => placeFinder());
  observer.observe(host);
  let watched: Element | null = null;
  const watchStage = () => {
    const st = document.querySelector("#preview .stage");
    if (st && st !== watched) { observer.observe(st); watched = st; }
  };
  watchStage();
  // Once a frame, however many changes land in it: the stage has its new size by then.
  let pending = 0;
  store.select((s) => [s.activity, s.sequence, s.mode, s.project], () => {
    watchStage();
    if (!pending) pending = requestAnimationFrame(() => { pending = 0; placeFinder(); });
  });

  // ---- caption: the agent's last edit, in its own words
  const caption = el("div", "den-caption");
  const capRev = el("span", "rev");
  const capText = el("span", "text");
  caption.append(capRev, capText);
  host.appendChild(caption);
  store.select((s) => [s.history, s.activity], (s) => {
    const h = s.history.find((x) => x.author === "agent" && x.kind === "edit" && x.summary);
    const a = s.activity.find((x) => x.summary);
    const rev = h?.revision ?? a?.revision_after ?? null;
    const text = h?.summary ?? a?.summary ?? "";
    caption.hidden = !text;
    capRev.textContent = rev !== null ? `r${rev}` : "";
    capText.textContent = text;
  });

  // ---- the rail
  const rail = el("div", "den-rail");
  const play = playButton("den-play");
  const now = el("span", "den-time");
  const end = el("span", "den-time end");
  const track = el("div", "den-track");
  const ticks = el("div", "dr-ticks");
  const marks = el("div", "dr-marks");
  const head = el("div", "dr-head");
  track.append(ticks, marks, head);
  const keys = el("div", "den-keys");
  // The panels that have a key, in the order they were registered.
  keys.append(...panelKeys(panels.all.filter((spec) => spec.key), "den-key", (b, spec) => { b.title = `${spec.title} (${keysFor(`panel.${spec.id}`)})`; }));
  const mark = button("", () => run(Cmd.addMarker), `A marker at the playhead: a note on this frame for the agent, or for you (${keysFor(Cmd.addMarker)})`, "den-marker");
  mark.append(el("span", "dot"), "marker");
  rail.append(play, now, track, end, keys, mark);
  host.appendChild(rail);

  // The sequence's length, taken once per redraw: `store.duration` walks every clip.
  let d = 1;
  const share = (t: number) => (Math.min(Math.max(0, t), d) / d) * 100;
  const x = (t: number) => `${share(t)}%`;
  track.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    const r = track.getBoundingClientRect();
    const seek = (cx: number) => store.seek(((cx - r.left) / r.width) * d);
    seek(e.clientX);
    dragPointer(e, { axes: "x", move: (_dx, _dy, ev) => seek(ev.clientX), end: () => {} });
  });

  // Hidden in power, so drawn only in den — and all of it again on the way in.
  const den = () => store.state.mode === "den";
  const drawHead = (s = store.state) => {
    if (!den()) return;
    setText(now, fmtTime(s.playhead, store.fps));
    // The head spans the track and moves by a transform: a share of its own width, which is
    // the track's, so the window is not laid out again 25 times a second.
    head.style.transform = `translateX(${share(s.playhead)}%)`;
  };
  store.select((s) => [s.playhead], drawHead);
  let ticked = 0;
  store.select((s) => [s.project, s.sequence, s.history, s.markerList, s.mode], (s) => {
    if (!den()) return;
    d = Math.max(store.duration, 1);
    end.textContent = fmtTime(d, store.fps);
    drawHead(s);
    // A tick a second, a taller one every ten — denser and it is a texture, not a scale.
    // They depend on the length alone.
    if (d !== ticked) {
      ticked = d;
      const step = d > 600 ? 10 : d > 120 ? 5 : 1;
      ticks.replaceChildren();
      for (let t = 0; t <= d; t += step) {
        const tk = el("span", (t / step) % 10 === 0 ? "big" : "");
        tk.style.left = x(t);
        ticks.appendChild(tk);
      }
    }
    marks.replaceChildren();
    // The last revisions that edited something on this sequence, where they edited it.
    const starts = new Map(store.tracks.flatMap((t) => t.clips).map((c) => [c.id, c.start]));
    const placed: { h: HistoryEntry; t: number }[] = [];
    for (const h of s.history) {
      if (placed.length === 3) break;
      if (h.revision === null || h.kind === "refused") continue;
      const t = editedAt(h, starts);
      if (t !== null) placed.unshift({ h, t });
    }
    placed.forEach(({ h, t }, i) => {
      const m = el("div", `dr-rev${i === placed.length - 1 ? " latest" : ""}`);
      m.style.left = x(t);
      m.append(el("span", "", `r${h.revision}`));
      m.title = `r${h.revision} · ${h.summary}`;
      m.onclick = (e) => { e.stopPropagation(); store.seek(t); };
      m.onpointerdown = (e) => e.stopPropagation();
      marks.appendChild(m);
    });
    // Every marker; the agent's own in its colour, so a note it left itself is not one for it.
    for (const m of markersOn(s.markerList, s.sequence)) {
      const p = el("div", `dr-marker${m.author === "agent" ? " agent" : ""}`);
      p.style.left = x(m.t);
      p.title = m.label;
      p.onpointerdown = (e) => e.stopPropagation();
      p.onclick = () => store.seek(m.t);
      marks.appendChild(p);
    }
  });
}

/** The agent's last look at what is on screen: a sample of the open sequence (the master
 *  when it named none), not of an overlay on its own. */
function lastLook(activity: Activity[], sequence: string, master: string): Activity | undefined {
  return activity.find((a) => a.tool === "sample_frames" && a.sample && !a.sample.overlay && (a.sample.sequence ?? master) === sequence);
}

/** Where on the open sequence a revision edited: the earliest start of the clips it
 *  touched there (`starts`, by clip id), as they are now. Null when it touched nothing on
 *  this timeline. */
function editedAt(h: HistoryEntry, starts: Map<string, number>): number | null {
  const at = h.targets.map((id) => starts.get(id)).filter((t): t is number => t !== undefined);
  return at.length ? Math.min(...at) : null;
}
